# Claude Code OpenTelemetry Integration Plan

**Date**: 2026-01-03
**Branch**: claudecode/otel
**Strategy**: Simplified session_id-based correlation
**Research**: `research/2026-01-03_01-10-02_claude-code-otel-integration.md`

## Overview

Enable OpenTelemetry observability for Claude Code sessions by combining native OTEL exports with custom hook-based chat logs. Use `session_id` as the correlation key to join native telemetry with detailed conversation transcripts, avoiding the complexity of trace context injection.

## Current State Analysis

### What Exists

**Native OTEL Support** (Built into Claude Code):
- Metrics: Token usage, costs, command duration, success rates
- Logs: User prompts, tool results, API requests
- Export: OTLP protocol (HTTP/protobuf or gRPC)
- Configuration: Environment variables
- **Status**: Available but not enabled

**Custom Hook System** (Already implemented):
- Hook-based event logging to `logs/*.json`
- Session lifecycle events (SessionStart, UserPromptSubmit, etc.)
- Chat transcript creation via `--chat` flag
- **Correlation Field**: `session_id` present in all logs
- **File**: `~/.claude/hooks/session_start.py:24-46`

**Log Files Created**:
- `logs/session_start.json` - Session initialization
- `logs/user_prompt_submit.json` - User prompts
- `logs/pre_tool_use.json` - Pre-tool execution
- `logs/post_tool_use.json` - Post-tool execution
- `logs/chat.json` - Full conversation transcript (optional with `--chat`)
- `logs/subagent_stop.json` - Subagent completion
- `logs/stop.json` - Session stop events

### Key Discoveries

✅ **session_id already exists** in all hook logs:
```json
{
  "session_id": "79cb6874-fb9e-4c66-be38-1e4948a8494d",
  "cwd": "/Users/stevengonsalvez/d/git/agents-in-a-box",
  "hook_event_name": "SessionStart"
}
```

✅ **No code changes needed** - Hooks already log session_id

❌ **No trace_id/span_id** - Not currently captured (and we're not adding it)

✅ **Simple correlation strategy** - Join on session_id field

### What's Missing

1. Native OTEL not enabled (environment variables not set)
2. No OTEL Collector to ingest hook logs
3. No backend for visualization (Jaeger/Grafana)
4. No session_id in native OTEL resource attributes

## Desired End State

### Architecture

```
┌──────────────────────────────────────┐
│   Claude Code (Native OTEL)          │
│   - Metrics: tokens, costs, perf     │
│   - Logs: prompts, tool results      │
│   - Resource: session.id added       │
└─────────────┬────────────────────────┘
              │ OTLP
              ▼
    ┌─────────────────────┐
    │  OTEL Collector     │
    │  - Receives OTLP    │
    │  - Tails hook logs  │
    │  - Adds session.id  │
    └─────────┬───────────┘
              │ OTLP
              ▼
    ┌─────────────────────┐
    │  Jaeger/Grafana     │
    │  Backend            │
    │  - Stores metrics   │
    │  - Stores logs      │
    │  - Query by session │
    └─────────────────────┘
              ▲
              │ Filelog
    ┌─────────┴───────────┐
    │  Hook Logs          │
    │  logs/*.json        │
    │  - session_id field │
    └─────────────────────┘
```

### Correlation Flow

1. **Native OTEL**: Exports with `session.id` resource attribute
2. **Hook Logs**: Already contain `session_id` field
3. **OTEL Collector**: Ingests both, normalizes `session_id` → `session.id`
4. **Backend Query**: Join on `session.id` to correlate

### Success Criteria

#### Automated Verification

- [ ] Environment variables set in shell config (`~/.zshrc` or `~/.bashrc`)
- [ ] Jaeger container running: `docker ps | grep jaeger`
- [ ] OTEL Collector container running: `docker ps | grep otel`
- [ ] Native OTEL exports visible: `curl http://localhost:16686/api/services` shows "claude-code"
- [ ] Hook logs ingested: Jaeger shows logs from `logs/*.json`
- [ ] Session correlation works: Query by session.id returns both native + hook data

#### Manual Verification

- [ ] Start Claude Code session - verify telemetry exports to Jaeger
- [ ] Open Jaeger UI (http://localhost:16686) - see metrics and logs
- [ ] Search by session.id - find both native OTEL and hook logs
- [ ] Verify chat.json correlation - conversation context appears with telemetry
- [ ] Check token usage metrics - costs and usage tracked correctly
- [ ] Verify multi-session isolation - different sessions show separate data

## What We're NOT Doing

❌ **No trace_id/span_id injection** - Too complex, session_id is sufficient
❌ **No hook code modifications** - Works as-is
❌ **No distributed tracing** - Just metrics + logs correlation
❌ **No production deployment** - Local Jaeger only for now
❌ **No custom OTEL processors** - Using standard Collector components
❌ **No log sampling** - All logs collected initially

## Implementation Approach

Use a **phased rollout** approach:
1. **Phase 1**: Enable native OTEL → Local Jaeger (validate basics)
2. **Phase 2**: Add OTEL Collector → Ingest hook logs (validate correlation)
3. **Phase 3**: Test queries and dashboards (validate usefulness)
4. **Phase 4** (Future): Production deployment options

---

## Phase 1: Enable Native OTEL with Jaeger

### Overview

Enable Claude Code's native OpenTelemetry exports and verify they reach a local Jaeger instance. This validates the basic OTEL pipeline without hook log correlation.

### Changes Required

#### 1. Start Jaeger Backend

**Command**:
```bash
docker run -d --name jaeger \
  -e COLLECTOR_OTLP_ENABLED=true \
  -p 16686:16686 \
  -p 4317:4317 \
  -p 4318:4318 \
  jaegertracing/all-in-one:latest
```

**What this does**:
- Port 16686: Jaeger UI
- Port 4317: OTLP gRPC receiver
- Port 4318: OTLP HTTP receiver

**Verify**:
```bash
# Check container is running
docker ps | grep jaeger

# Access UI
open http://localhost:16686
```

#### 2. Configure Environment Variables

**File**: `~/.zshrc` (or `~/.bashrc` for bash users)

**Add**:
```bash
# Claude Code OpenTelemetry Configuration
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL="grpc"
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"

# Optional: Add session context as resource attribute
# Note: This requires setting SESSION_ID before each claude invocation
# export OTEL_RESOURCE_ATTRIBUTES="environment=development,user=stevengonsalvez"
```

**Apply changes**:
```bash
source ~/.zshrc  # or source ~/.bashrc
```

#### 3. Test Native OTEL Export

**Run Claude Code**:
```bash
cd /Users/stevengonsalvez/d/git/agents-in-a-box
claude --help  # Any Claude Code command
```

**Verify in Jaeger**:
1. Open http://localhost:16686
2. Select service: "claude-code"
3. Click "Find Traces"
4. Should see metrics and logs from the session

### Success Criteria

#### Automated Verification

```bash
# Check Jaeger is running
docker ps | grep jaeger | grep -q "Up" && echo "✅ Jaeger running" || echo "❌ Jaeger not running"

# Check environment variables are set
env | grep -q "CLAUDE_CODE_ENABLE_TELEMETRY=1" && echo "✅ Telemetry enabled" || echo "❌ Telemetry not enabled"

# Check OTEL exports (after running Claude Code)
curl -s http://localhost:16686/api/services | grep -q "claude-code" && echo "✅ Native OTEL working" || echo "❌ No OTEL data"
```

#### Manual Verification

- [ ] Jaeger UI loads at http://localhost:16686
- [ ] "claude-code" appears in service dropdown
- [ ] Metrics show token usage and costs
- [ ] Logs show user prompts and tool results
- [ ] Performance data (P50/P95/P99) visible

### Rollback Plan

If issues occur:
```bash
# Stop Jaeger
docker stop jaeger && docker rm jaeger

# Remove environment variables
# Edit ~/.zshrc and remove OTEL lines, then:
source ~/.zshrc
```

---

## Phase 2: Add OTEL Collector for Hook Logs

### Overview

Deploy OpenTelemetry Collector to ingest hook logs from `logs/*.json` files and forward them to Jaeger alongside native OTEL data. Configure session_id correlation.

### Changes Required

#### 1. Create OTEL Collector Configuration

**File**: `/Users/stevengonsalvez/d/git/agents-in-a-box/otel-collector-config.yaml`

```yaml
receivers:
  # Receive native OTEL from Claude Code
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
      http:
        endpoint: 0.0.0.0:4318

  # Tail hook log files
  filelog:
    include:
      - /logs/*.json
    operators:
      # Parse JSON log files
      - type: json_parser
        parse_from: body

      # Extract timestamp
      - type: time_parser
        if: 'body.timestamp != nil'
        parse_from: body.timestamp
        layout: '%Y-%m-%dT%H:%M:%S'

processors:
  # Batch for efficiency
  batch:
    timeout: 10s
    send_batch_size: 100

  # Add resource attributes
  resource:
    attributes:
      - key: service.name
        value: claude-code
        action: upsert
      - key: service.version
        value: 4.5
        action: upsert
      - key: environment
        value: development
        action: upsert

  # Transform to normalize session_id field
  transform:
    log_statements:
      - context: log
        statements:
          # Normalize session_id to session.id resource attribute
          - set(resource.attributes["session.id"], body.session_id) where body.session_id != nil
          - set(resource.attributes["session.id"], body.sessionId) where body.sessionId != nil

          # Add hook event name as attribute
          - set(attributes["hook.event"], body.hook_event_name) where body.hook_event_name != nil

          # Add user prompt content
          - set(attributes["user.prompt"], body.prompt) where body.prompt != nil

exporters:
  # Console for debugging
  debug:
    verbosity: detailed
    sampling_initial: 5
    sampling_thereafter: 200

  # Jaeger for visualization
  otlp/jaeger:
    endpoint: http://jaeger:4317
    tls:
      insecure: true

service:
  pipelines:
    # Native OTEL pipeline
    logs:
      receivers: [otlp]
      processors: [batch, resource]
      exporters: [debug, otlp/jaeger]

    metrics:
      receivers: [otlp]
      processors: [batch, resource]
      exporters: [otlp/jaeger]

    # Hook logs pipeline
    logs/hooks:
      receivers: [filelog]
      processors: [batch, resource, transform]
      exporters: [debug, otlp/jaeger]
```

#### 2. Create Docker Compose Setup

**File**: `/Users/stevengonsalvez/d/git/agents-in-a-box/docker-compose-otel.yaml`

```yaml
version: '3.8'

services:
  jaeger:
    image: jaegertracing/all-in-one:latest
    container_name: jaeger
    environment:
      - COLLECTOR_OTLP_ENABLED=true
    ports:
      - "16686:16686"  # Jaeger UI
      - "4317:4317"    # OTLP gRPC
      - "4318:4318"    # OTLP HTTP
    networks:
      - otel

  otel-collector:
    image: otel/opentelemetry-collector-contrib:latest
    container_name: otel-collector
    command: ["--config=/etc/otel-collector-config.yaml"]
    volumes:
      - ./otel-collector-config.yaml:/etc/otel-collector-config.yaml:ro
      - ./logs:/logs:ro
    ports:
      - "4317:4317"    # OTLP gRPC (for Claude Code)
      - "4318:4318"    # OTLP HTTP
    depends_on:
      - jaeger
    networks:
      - otel

networks:
  otel:
    driver: bridge
```

#### 3. Update Claude Code OTEL Config

**File**: `~/.zshrc`

**Change**:
```bash
# OLD - Direct to Jaeger
# export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"

# NEW - Route through OTEL Collector
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
# (No change needed - Collector proxies to Jaeger)
```

#### 4. Start the Stack

**Commands**:
```bash
cd /Users/stevengonsalvez/d/git/agents-in-a-box

# Start Jaeger + OTEL Collector
docker-compose -f docker-compose-otel.yaml up -d

# Verify both containers are running
docker-compose -f docker-compose-otel.yaml ps

# View collector logs
docker-compose -f docker-compose-otel.yaml logs -f otel-collector
```

### Success Criteria

#### Automated Verification

```bash
# Check containers are running
docker ps | grep -q "otel-collector" && echo "✅ Collector running" || echo "❌ Collector not running"
docker ps | grep -q "jaeger" && echo "✅ Jaeger running" || echo "❌ Jaeger not running"

# Check collector can read logs
docker exec otel-collector ls /logs/*.json && echo "✅ Log files accessible" || echo "❌ Log files not accessible"

# Run a Claude Code session and check correlation
# (Manual step - automated check would query Jaeger API)
```

#### Manual Verification

- [ ] Both containers running: `docker-compose -f docker-compose-otel.yaml ps`
- [ ] Collector ingesting native OTEL from Claude Code
- [ ] Collector tailing hook logs from `logs/*.json`
- [ ] Jaeger UI shows both native metrics AND hook logs
- [ ] Search by session.id returns correlated data
- [ ] Hook event types visible (SessionStart, UserPromptSubmit, etc.)

### Rollback Plan

```bash
# Stop all containers
docker-compose -f docker-compose-otel.yaml down

# Remove volumes (if needed)
docker-compose -f docker-compose-otel.yaml down -v

# Revert to Phase 1 setup (direct Jaeger)
docker run -d --name jaeger \
  -e COLLECTOR_OTLP_ENABLED=true \
  -p 16686:16686 -p 4317:4317 -p 4318:4318 \
  jaegertracing/all-in-one:latest
```

---

## Phase 3: Test Queries and Validation

### Overview

Validate that session_id correlation works correctly by running test sessions and querying Jaeger for correlated data.

### Test Scenarios

#### Test 1: Single Session Correlation

**Steps**:
1. Start a new Claude Code session:
   ```bash
   cd /Users/stevengonsalvez/d/git/agents-in-a-box
   claude "Run /research test query"
   ```

2. Note the session_id from `logs/session_start.json`:
   ```bash
   cat logs/session_start.json | jq '.[-1].session_id'
   ```

3. Query Jaeger by session.id:
   - Open http://localhost:16686
   - Service: "claude-code"
   - Tags: `session.id=<your-session-id>`
   - Click "Find Traces"

**Expected**:
- Native OTEL logs: User prompts, tool results, API calls
- Hook logs: SessionStart, UserPromptSubmit, Stop events
- All have matching session.id

#### Test 2: Multi-Session Isolation

**Steps**:
1. Run 3 different Claude Code sessions
2. Each should have unique session_id
3. Query Jaeger for each session.id individually

**Expected**:
- Each query returns only data for that specific session
- No cross-contamination between sessions
- Session timestamps align with actual usage

#### Test 3: Chat Log Correlation

**Steps**:
1. Run a session with `--chat` flag:
   ```bash
   claude "Help me test OTEL" --chat
   ```

2. Verify `logs/chat.json` created:
   ```bash
   ls -lh logs/chat.json
   ```

3. Check Jaeger for chat message content

**Expected**:
- Chat messages appear in Jaeger logs
- User/assistant messages correlated by session.id
- Token usage visible in chat log entries

### Validation Queries

**Jaeger UI Queries**:

```
# Find all sessions in last hour
service=claude-code
operation=*
lookback=1h

# Find specific session
service=claude-code
session.id=79cb6874-fb9e-4c66-be38-1e4948a8494d

# Find all user prompts
service=claude-code
hook.event=UserPromptSubmit

# Find high-cost sessions
service=claude-code
cost>10
```

**API Queries** (for automation):
```bash
# Get all services
curl http://localhost:16686/api/services

# Get traces with session.id tag
curl "http://localhost:16686/api/traces?service=claude-code&tags=%7B%22session.id%22%3A%22SESSION_ID%22%7D"

# Get operations
curl "http://localhost:16686/api/services/claude-code/operations"
```

### Success Criteria

#### Automated Verification

```bash
# Script to validate correlation
#!/bin/bash
SESSION_ID=$(cat logs/session_start.json | jq -r '.[-1].session_id')

# Query Jaeger API
RESPONSE=$(curl -s "http://localhost:16686/api/traces?service=claude-code&tags=%7B%22session.id%22%3A%22${SESSION_ID}%22%7D")

# Check if response contains data
echo "$RESPONSE" | jq '.data | length' | grep -q '^[1-9]' && \
  echo "✅ Session correlation working" || \
  echo "❌ No correlated data found"
```

#### Manual Verification

- [ ] Session_id query returns both native + hook data
- [ ] Chat messages appear with conversation context
- [ ] Token usage and costs accurately tracked
- [ ] Tool execution events visible (pre/post)
- [ ] Timestamps align across native and hook logs
- [ ] Multi-session queries correctly isolated

---

## Phase 4: Production Deployment Options (Future)

### Overview

When ready to move beyond local development, choose a production backend for OTEL data.

### Backend Options

#### Option 1: Grafana Cloud (Recommended)

**Pros**:
- Managed service, no infrastructure
- Native OTEL support
- Free tier available
- Built-in dashboards

**Setup**:
```bash
# Update environment variables
export OTEL_EXPORTER_OTLP_ENDPOINT="https://otlp-gateway-prod-eu-north-0.grafana.net/otlp"
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic $(echo -n 'user:password' | base64)"
export OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf"
```

#### Option 2: Self-Hosted Jaeger

**Pros**:
- Full control
- No data leaves your infrastructure
- No costs

**Setup**:
- Deploy Jaeger to Kubernetes or cloud VM
- Configure persistent storage
- Setup ingress for UI access

#### Option 3: Datadog

**Pros**:
- Enterprise features (APM, profiling)
- Advanced dashboards
- Alerting and anomaly detection

**Setup**:
```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="https://otlp.datadoghq.com"
export OTEL_EXPORTER_OTLP_HEADERS="dd-api-key=YOUR_DD_API_KEY"
export DD_SITE="datadoghq.com"
```

### Migration Steps

When ready for production:

1. **Choose backend** (Grafana/Datadog/Self-hosted)
2. **Update OTEL Collector config** (change exporter endpoint)
3. **Test with single session**
4. **Update environment variables** for team
5. **Document access URLs** and credentials
6. **Create dashboards** for key metrics

---

## Testing Strategy

### Unit Tests

Not applicable - this is infrastructure configuration, not code.

### Integration Tests

**Test 1: Native OTEL Export**
```bash
# Start Claude Code
claude "test prompt"

# Verify export
curl -s http://localhost:16686/api/services | grep -q "claude-code"
```

**Test 2: Hook Log Ingestion**
```bash
# Create a test log entry
echo '{"session_id":"test-123","hook_event_name":"Test"}' > logs/test.json

# Wait for ingestion (5-10 seconds)
sleep 10

# Query Jaeger
curl -s "http://localhost:16686/api/traces?service=claude-code&tags=%7B%22session.id%22%3A%22test-123%22%7D" | \
  jq '.data | length'
```

**Test 3: Session Correlation**
```bash
# Run full session
claude "/research test correlation"

# Extract session_id
SESSION_ID=$(cat logs/session_start.json | jq -r '.[-1].session_id')

# Query both native and hook data
curl -s "http://localhost:16686/api/traces?service=claude-code&tags=%7B%22session.id%22%3A%22${SESSION_ID}%22%7D" | \
  jq '.data[].spans | length' | \
  awk '{sum+=$1} END {print "Total spans:", sum}'
```

### Manual Testing Steps

1. **Fresh Session Test**:
   - Clear logs: `rm logs/*.json`
   - Start new session: `claude "test"`
   - Verify session_id in Jaeger
   - Check all log types appear

2. **Multi-Session Test**:
   - Run 5 concurrent Claude sessions
   - Verify each has unique session_id
   - Check no data mixing in Jaeger

3. **Chat Log Test**:
   - Run session with `--chat` flag
   - Verify chat.json created
   - Check conversation appears in Jaeger

4. **Performance Test**:
   - Run long session (20+ prompts)
   - Monitor OTEL Collector CPU/memory
   - Check Jaeger UI responsiveness

---

## Performance Considerations

### OTEL Collector Resource Usage

**Expected**:
- CPU: ~50-100m (idle), ~500m (active ingestion)
- Memory: ~128-256MB
- Disk I/O: Minimal (tailing logs)

**Monitoring**:
```bash
# Check collector resource usage
docker stats otel-collector

# Check collector health
curl http://localhost:13133/  # Health check endpoint
```

### Log File Growth

**Mitigation**:
- Implement log rotation for `logs/*.json`
- Archive old sessions periodically
- Consider OTEL Collector sampling for high-volume sessions

**Example Rotation Script**:
```bash
#!/bin/bash
# Rotate logs older than 7 days
find logs/ -name "*.json" -mtime +7 -exec mv {} logs/archive/ \;
```

### Jaeger Storage

**Local Development**:
- In-memory storage (ephemeral)
- Restarts lose data (acceptable for dev)

**Production**:
- Use Elasticsearch/Cassandra backend
- Configure retention policies
- Monitor storage usage

---

## Migration Notes

### Existing Log Files

**Backward Compatibility**:
- Old logs (without session.id) can still be queried
- OTEL Collector will attempt to extract session_id/sessionId
- Logs without session field appear as "unattributed"

**Cleanup**:
```bash
# Archive pre-OTEL logs
mkdir -p logs/archive/pre-otel
mv logs/*.json logs/archive/pre-otel/
```

### Team Rollout

**Phase 1**: Individual developers enable locally
**Phase 2**: Shared Jaeger instance for team
**Phase 3**: Production backend (Grafana Cloud)

**Communication**:
- Document environment variable setup in team wiki
- Share Jaeger UI URL
- Create example queries for common use cases

---

## Troubleshooting Guide

### Problem: No data in Jaeger

**Diagnosis**:
```bash
# Check environment variables
env | grep CLAUDE_CODE_ENABLE_TELEMETRY
env | grep OTEL_

# Check containers running
docker ps | grep -E "jaeger|otel-collector"

# Check collector logs
docker-compose -f docker-compose-otel.yaml logs otel-collector | tail -50
```

**Solutions**:
- Verify `CLAUDE_CODE_ENABLE_TELEMETRY=1`
- Check Jaeger accepting OTLP on 4317
- Restart collector: `docker-compose restart otel-collector`

### Problem: Hook logs not appearing

**Diagnosis**:
```bash
# Check log files exist
ls -lh logs/*.json

# Check collector can access logs
docker exec otel-collector ls /logs

# Check filelog receiver in collector logs
docker-compose logs otel-collector | grep filelog
```

**Solutions**:
- Verify volume mount in docker-compose
- Check file permissions
- Ensure JSON is valid: `cat logs/session_start.json | jq .`

### Problem: session_id not correlating

**Diagnosis**:
```bash
# Check field name consistency
cat logs/session_start.json | jq '.[0] | keys | map(select(contains("session")))'

# Check Jaeger tag format
curl "http://localhost:16686/api/traces?service=claude-code" | \
  jq '.data[0].spans[0].tags[] | select(.key | contains("session"))'
```

**Solutions**:
- Verify transform processor in collector config
- Check both `session_id` and `sessionId` variants handled
- Update OTTL statements if needed

---

## References

### Research Documents
- `research/2026-01-03_01-10-02_claude-code-otel-integration.md` - Full research findings

### Configuration Files
- `/Users/stevengonsalvez/d/git/agents-in-a-box/otel-collector-config.yaml` - Collector config
- `/Users/stevengonsalvez/d/git/agents-in-a-box/docker-compose-otel.yaml` - Docker Compose setup
- `~/.zshrc` - Environment variable configuration

### Hook Files
- `~/.claude/hooks/session_start.py:24-46` - Session event logging
- `~/.claude/hooks/stop.py:183-203` - Chat log creation
- `~/.claude/hooks/user_prompt_submit.py` - User prompt logging
- `~/.claude/hooks/post_tool_use.py:46-77` - Tool usage logging

### External Documentation
- [OpenTelemetry Collector Documentation](https://opentelemetry.io/docs/collector/)
- [Filelog Receiver](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/receiver/filelogreceiver/README.md)
- [Jaeger Getting Started](https://www.jaegertracing.io/docs/latest/getting-started/)
- [OTTL Language Reference](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/pkg/ottl)

---

## Quick Start Guide

For the impatient (summarized from all phases):

```bash
# 1. Start the stack
cd /Users/stevengonsalvez/d/git/agents-in-a-box
docker-compose -f docker-compose-otel.yaml up -d

# 2. Enable OTEL in shell config
echo '
# Claude Code OpenTelemetry
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL="grpc"
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
' >> ~/.zshrc

source ~/.zshrc

# 3. Run Claude Code
claude "test OTEL integration"

# 4. Open Jaeger UI
open http://localhost:16686
# Service: claude-code
# Look for traces with session.id tags

# 5. Query by session ID
SESSION_ID=$(cat logs/session_start.json | jq -r '.[-1].session_id')
# In Jaeger UI: Tags → session.id = <paste SESSION_ID>
```

Done! Your Claude Code sessions are now observable with native OTEL metrics and hook-based chat logs, all correlated by session_id.
