# Research: Claude Code OTEL Integration & Chat Log Correlation

**Date**: 2026-01-03T01:10:02+00:00
**Repository**: agents-in-a-box
**Branch**: claudecode/otel
**Commit**: 29f8d562614ca949a995750bae18714b9998b7fd
**Research Type**: Comprehensive (Codebase + Documentation + Web)

## Research Question

Can you research Claude Code's OTEL capabilities? I want to use that. Also, we create logs of our chat (like from the hooks - research and find out which hooks we create chats.json, etc.). How can that complement with the OTEL?

## Executive Summary

**CORRECTION**: Claude Code **DOES have native OpenTelemetry (OTEL) support**! It can export metrics and logs via OTLP (OpenTelemetry Protocol) to backends like Grafana Cloud, Datadog, Jaeger, and SigNoz.

**Native OTEL Capabilities**:
- Export metrics (token usage, costs, command duration, success rates)
- Export log events (user prompts, tool results, API requests)
- OTLP protocol support (HTTP/protobuf and gRPC)
- Environment variable configuration

**PLUS Custom Hook System**:
Claude Code also implements a custom event-based telemetry system using Python hooks that generate JSON log files in `logs/*.json`. These hooks capture session lifecycle events (SessionStart, UserPromptSubmit, SubagentStop, etc.).

**Integration Strategy**:
Combine native OTEL exports with custom hook-based chat logs by:
1. Enabling native OTEL metrics/logs export via environment variables
2. Enriching hook JSON logs with trace context (`trace_id`, `span_id`, `trace_flags`)
3. Using OTEL Collector's filelog receiver to ingest chat logs
4. Unified visualization in Jaeger/Grafana showing both native telemetry AND chat conversations

## Key Findings

### 1. Claude Code HAS Native OTEL Support

**Enable via Environment Variables**:
```bash
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf"
export OTEL_EXPORTER_OTLP_ENDPOINT="https://your-otlp-endpoint"
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic YOUR_TOKEN"
```

**What's Exported**:
- **Metrics**: Token usage (input/output), costs, command duration (P50/P95/P99), success rates, tool usage distribution
- **Logs**: User prompts, tool results, API requests, tool acceptance decisions
- **Protocol**: OTLP (HTTP/protobuf or gRPC)
- **Backends**: Grafana Cloud, Jaeger, Datadog, Honeycomb, SigNoz

**PLUS Custom Hooks**: You also have a rich hook system that creates `logs/*.json` files for session events, which can be correlated with native OTEL traces

### 2. Chat Logs Are Created by Hooks (Optional)
- **Primary Hook**: `stop.py` and `subagent_stop.py` with `--chat` flag
- **Output Location**: `logs/chat.json`
- **Source Data**: Converts `.jsonl` transcript files to JSON arrays
- **Data Format**: Array of message objects with role, content, and token usage

### 3. OpenTelemetry Can Unify Chat Logs and Telemetry
- **Correlation Mechanism**: Add `trace_id`, `span_id`, `trace_flags` to chat logs
- **Integration Path**: Use OTEL Collector's filelog receiver + transform processor
- **Benefit**: Bidirectional navigation between traces and chat conversations
- **Standard**: W3C Trace Context format for interoperability

---

## Detailed Findings

### Codebase Analysis

#### 1. Hook System Architecture

**File**: `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/claude-code-4.5/settings.json:22-88`

Claude Code uses a hook-based telemetry system with the following events:

| Event | Handler | Purpose | Output |
|-------|---------|---------|--------|
| **SessionStart** | `session_start.py` | Session initialization | `logs/session_start.json` |
| **UserPromptSubmit** | `user_prompt_submit.py` | Every user prompt | `logs/user_prompt_submit.json` |
| **PreToolUse** | `pre_tool_use.py` | Before tool execution | `logs/pre_tool_use.json` |
| **PostToolUse** | `post_tool_use.py` | After tool execution | `logs/post_tool_use.json` |
| **SubagentStop** | `subagent_stop.py` | Subagent completion | `logs/subagent_stop.json` |
| **Stop** | `stop.py` | Main session end | `logs/stop.json` |
| **PreCompact** | `pre_compact.py` | Before context compaction | `logs/pre_compact.json` |
| **Notification** | `notification.py` | Waiting for input | `logs/notification.json` |

#### 2. Chat Log Creation Hooks

**File**: `/Users/stevengonsalvez/.claude/hooks/stop.py:183-203`

```python
if args.chat and 'transcript_path' in input_data:
    transcript_path = input_data['transcript_path']
    if os.path.exists(transcript_path):
        chat_data = []
        try:
            with open(transcript_path, 'r') as f:
                for line in f:
                    line = line.strip()
                    if line:
                        try:
                            chat_data.append(json.loads(line))
                        except json.JSONDecodeError:
                            pass

            chat_file = os.path.join(log_dir, 'chat.json')
            with open(chat_file, 'w') as f:
                json.dump(chat_data, f, indent=2)
```

**Trigger**: Session end with `--chat` flag
**Input**: `.jsonl` transcript file (one JSON object per line)
**Output**: `logs/chat.json` (JSON array)
**Identical Logic**: `subagent_stop.py:115-136` has the same implementation

**Example chat.json Structure**:
```json
[
  {
    "timestamp": "2026-01-03T...",
    "message": {
      "role": "user",
      "content": "Can you research on claude code OTEL capabilities...",
      "usage": {
        "input_tokens": 1500,
        "output_tokens": 200
      }
    }
  },
  {
    "timestamp": "2026-01-03T...",
    "message": {
      "role": "assistant",
      "content": "I'm ready to conduct comprehensive research...",
      "usage": {
        "input_tokens": 1700,
        "output_tokens": 500
      }
    }
  }
]
```

#### 3. Session Event Logging

**File**: `/Users/stevengonsalvez/.claude/hooks/session_start.py:24-46`

```python
def log_session_start(input_data):
    """Log session start event to logs directory."""
    log_dir = Path("logs")
    log_dir.mkdir(parents=True, exist_ok=True)
    log_file = log_dir / 'session_start.json'

    if log_file.exists():
        with open(log_file, 'r') as f:
            log_data = json.load(f)
    else:
        log_data = []

    log_data.append(input_data)

    with open(log_file, 'w') as f:
        json.dump(log_data, f, indent=2)
```

**Example session_start.json**:
```json
{
  "session_id": "79cb6874-fb9e-4c66-be38-1e4948a8494d",
  "transcript_path": "/Users/stevengonsalvez/.claude/projects/...jsonl",
  "cwd": "/Users/stevengonsalvez/d/git/agents-in-a-box",
  "hook_event_name": "SessionStart",
  "source": "startup"
}
```

#### 4. Tool Usage Logging

**File**: `/Users/stevengonsalvez/.claude/hooks/post_tool_use.py:46-77`

```python
# Line 65 in post_tool_use.py
input_data['logged_at'] = datetime.now().isoformat()
log_data.append(input_data)
```

**Logged Data**:
- Tool name
- Tool input parameters
- Tool output/results
- Execution timestamp
- Project context (MD5 hash of working directory)

**Special Features**:
- TodoWrite reflection with supervisor prompts
- Hashes todo list changes to avoid redundant prompts
- Writes to both `logs/post_tool_use.json` and `/tmp/claude_supervisor_{hash}.log`

#### 5. Current Logging Infrastructure

**File**: `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/workflows/multi-agent/utils/orchestrator-runner.sh:50-70`

Current logging uses simple Bash functions:

```bash
log_info() {
    echo "ℹ️  $*"
}

log_success() {
    echo "✅ $*"
}

log_error() {
    echo "❌ $*" >&2
}
```

**Format**: Text output with emoji prefixes
**Storage**: stdout/stderr (no persistence)
**No structured logging**: Pure text, no JSON or trace context

**File**: `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/workflows/multi-agent/orchestration/state/config.json`

```json
{
  "monitoring": {
    "check_interval_seconds": 30,
    "log_level": "info",
    "enable_cost_tracking": true
  }
}
```

**Current Telemetry Elements**:
- Cost tracking (session-level)
- Timing instrumentation for wave execution
- Token usage tracking (via statusline.js)
- No distributed tracing, no OTEL exporters

---

### OpenTelemetry Integration Patterns

#### 1. Log-to-Trace Correlation Mechanism

**Source**: [OpenTelemetry Logging Specification](https://opentelemetry.io/docs/specs/otel/logs/)

OpenTelemetry enables correlation through three dimensions:

1. **Time-based correlation**: Timestamps align logs with traces/metrics
2. **Execution context correlation**: `TraceId` and `SpanId` link logs to specific spans
3. **Resource context correlation**: Shared resource attributes (e.g., Kubernetes pod)

**Core trace context fields**:
- `TraceId`: Identifies the distributed trace (hex-encoded)
- `SpanId`: References the specific span within that trace (hex-encoded)
- `TraceFlags`: Metadata about trace characteristics (W3C format)

#### 2. Trace Context Field Standards for JSON Logs

**Source**: [Trace Context in Non-OTLP Log Formats](https://opentelemetry.io/docs/reference/specification/compatibility/logging_trace_context/)

**Standardized field names** (all lowercase):
```json
{
  "timestamp": 1581385157.14429,
  "body": "Incoming request",
  "trace_id": "102981abcd2901",
  "span_id": "abcdef1010",
  "trace_flags": "01"
}
```

Fields SHOULD be **top-level** in JSON structure for maximum compatibility.

#### 3. Automatic Correlation via Log Bridges/Appenders

**Source**: [Logs Bridge API Specification](https://opentelemetry.io/docs/reference/specification/logs/bridge-api/)

The **Logs Bridge API** allows logging libraries to automatically inject trace context:

**Supported frameworks**:
- **Python**: `logging` module
- **JavaScript/Node.js**: Winston, Pino
- **Go**: Zap, slog, log
- **Java**: SLF4J, Log4j, Logback

**Example: Python Manual Injection** (when bridge unavailable):
```python
from opentelemetry import trace
import structlog

logger = structlog.get_logger()
span = trace.get_current_span()
span_ctx = span.get_span_context()

logger.info("processing request",
    trace_id=format(span_ctx.trace_id, '032x'),
    span_id=format(span_ctx.span_id, '016x'),
)
```

#### 4. File-Based Log Integration with Filelog Receiver

**Source**: [Filelog Receiver README](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/receiver/filelogreceiver/README.md)

The OTEL Collector can tail JSON log files and parse them:

```yaml
receivers:
  filelog:
    include: [ /var/log/myservice/*.json ]
    operators:
      - type: json_parser
        timestamp:
          parse_from: attributes.time
          layout: '%Y-%m-%d %H:%M:%S'
```

**Key features**:
- Continuously tails log files
- Multi-stage parsing with operator pipelines
- Extracts timestamps and severity levels
- Maintains stateful offsets

#### 5. Collector Transformation with OTTL

**Source**: [Transforming Telemetry](https://opentelemetry.io/docs/collector/transforming-telemetry/)

**OpenTelemetry Transformation Language (OTTL)** enables custom enrichment:

```yaml
transform:
  log_statements:
    - statements:
        - merge_maps(log.cache, ParseJSON(log.body), "upsert")
          where IsMatch(log.body, "^\\{")
        - set(log.attributes["attr1"], log.cache["attr1"])
```

**Use cases**:
- Parse JSON bodies and promote fields to attributes
- Conditional filtering based on resource/attribute values
- Set severity levels based on content
- Add custom enrichment metadata

---

## Native Claude Code OTEL Configuration

### Enabling Built-in OTEL Export

Claude Code has native OpenTelemetry support that can be enabled via environment variables. This exports metrics and logs directly to OTLP-compatible backends.

#### Step 1: Set Environment Variables

**For Grafana Cloud**:
```bash
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf"
export OTEL_EXPORTER_OTLP_ENDPOINT="https://otlp-gateway-prod-eu-north-0.grafana.net/otlp"
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic YOUR_TOKEN"
export OTEL_METRIC_EXPORT_INTERVAL=10000  # milliseconds
export OTEL_LOGS_EXPORT_INTERVAL=5000     # milliseconds
```

**For Local Jaeger**:
```bash
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL="grpc"
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
```

**For Datadog**:
```bash
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_ENDPOINT="https://otlp.datadoghq.com"
export OTEL_EXPORTER_OTLP_HEADERS="dd-api-key=YOUR_DD_API_KEY"
export DD_SITE="datadoghq.com"
```

#### Step 2: Start Claude Code

Once environment variables are set, Claude Code will automatically export telemetry:

```bash
# Variables are set in your shell or .bashrc/.zshrc
claude --help  # Any Claude Code command will now export telemetry
```

#### Step 3: What Gets Exported

**Metrics (OTLP)**:
- **Token Usage**: Input/output tokens per session
- **Costs**: USD cost tracking
- **Command Duration**: P50, P95, P99 percentiles
- **Success Rates**: Command success/failure rates
- **Tool Distribution**: Which tools are used most (Read, Edit, Bash, etc.)
- **Model Selection**: Which models are invoked
- **Developer Decisions**: Acceptance/rejection patterns
- **Quota Consumption**: 5-hour rolling window usage

**Log Events (OTLP)**:
1. **User Prompts**: Developer requests and frequency
2. **Tool Results**: Execution outcomes and decisions
3. **API Requests**: Model calls with cost/performance data
4. **Tool Decisions**: Acceptance patterns indicating trust

#### Step 4: Visualize in Your Backend

**Grafana Cloud**: Create dashboards for token usage, costs, and performance
**Jaeger**: View traces and spans (if traces are enabled)
**Datadog**: Use APM dashboards with custom metrics

---

## Integration Architecture: Combining Native OTEL + Custom Chat Logs

### Two-Tier Telemetry Strategy

**Tier 1: Native OTEL** (Handled by Claude Code)
- Metrics: Token usage, costs, performance
- Logs: User prompts, tool results, API requests
- Export: OTLP to Grafana/Datadog/Jaeger

**Tier 2: Custom Chat Logs** (Your hooks system)
- Detailed conversation transcripts (`logs/chat.json`)
- Session lifecycle events (`logs/session_start.json`, etc.)
- Tool execution details (`logs/pre_tool_use.json`, `logs/post_tool_use.json`)

**Integration Goal**: Correlate native OTEL traces with custom chat logs by adding trace context to hook outputs.

### Proposed Implementation Strategy

#### Phase 1: Add Trace Context to Hooks

**Modify**: `/Users/stevengonsalvez/.claude/hooks/session_start.py`

```python
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import ConsoleSpanExporter, BatchSpanProcessor

# Initialize OTEL SDK
trace.set_tracer_provider(TracerProvider())
tracer = trace.get_tracer(__name__)

def log_session_start(input_data):
    # Create a span for session start
    with tracer.start_as_current_span("session.start") as span:
        span_ctx = span.get_span_context()

        # Add trace context to log
        input_data['trace_id'] = format(span_ctx.trace_id, '032x')
        input_data['span_id'] = format(span_ctx.span_id, '016x')
        input_data['trace_flags'] = format(span_ctx.trace_flags, '02x')

        # ... existing logging code ...
```

**Apply to all hooks**:
- `user_prompt_submit.py` - Span per user prompt
- `pre_tool_use.py` / `post_tool_use.py` - Span per tool execution
- `subagent_stop.py` - Span for subagent lifecycle
- `stop.py` - Root span for entire session

**Result**: All JSON logs now contain trace context fields.

#### Phase 2: Configure OTEL Collector

**Create**: `otel-collector-config.yaml`

```yaml
receivers:
  # Tail all Claude Code log files
  filelog:
    include:
      - /Users/stevengonsalvez/d/git/agents-in-a-box/logs/*.json
    operators:
      - type: json_parser
        parse_from: body
        timestamp:
          parse_from: attributes.logged_at
          layout: '%Y-%m-%dT%H:%M:%S'

      # Extract trace context
      - type: move
        from: attributes.trace_id
        to: resource.trace_id
      - type: move
        from: attributes.span_id
        to: resource.span_id

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
      - key: service.version
        value: 4.5
      - key: environment
        value: development

  # Transform and enrich
  transform:
    log_statements:
      - context: log
        statements:
          # Extract session_id to resource
          - set(resource.attributes["session.id"], attributes["session_id"])
          # Extract user prompt as event
          - set(attributes["event.name"], "user.prompt") where attributes["hook_event_name"] == "UserPromptSubmit"

exporters:
  # Console for debugging
  debug:
    verbosity: detailed

  # Jaeger for trace visualization
  otlp/jaeger:
    endpoint: http://localhost:4317
    tls:
      insecure: true

  # File for backup
  file:
    path: /tmp/otel-logs.json

service:
  pipelines:
    logs:
      receivers: [filelog]
      processors: [batch, resource, transform]
      exporters: [debug, otlp/jaeger, file]
```

**Run the Collector**:
```bash
docker run -v $(pwd)/otel-collector-config.yaml:/etc/otel-collector-config.yaml \
  -v /Users/stevengonsalvez/d/git/agents-in-a-box/logs:/logs:ro \
  otel/opentelemetry-collector-contrib:latest \
  --config=/etc/otel-collector-config.yaml
```

#### Phase 3: Start Jaeger Backend

```bash
docker run -d --name jaeger \
  -e COLLECTOR_OTLP_ENABLED=true \
  -p 16686:16686 \
  -p 4317:4317 \
  -p 4318:4318 \
  jaegertracing/all-in-one:latest
```

**Access UI**: http://localhost:16686

#### Phase 4: Enhanced Chat Logs with Trace Context

**Modified chat.json structure**:
```json
[
  {
    "timestamp": "2026-01-03T01:10:02+00:00",
    "trace_id": "102981abcd290100000000000000001a",
    "span_id": "abcdef1010111213",
    "trace_flags": "01",
    "message": {
      "role": "user",
      "content": "Can you research on claude code OTEL capabilities...",
      "usage": {
        "input_tokens": 1500,
        "output_tokens": 200
      }
    },
    "session_id": "79cb6874-fb9e-4c66-be38-1e4948a8494d",
    "hook_event_name": "UserPromptSubmit"
  }
]
```

**Benefits**:
1. Click on trace in Jaeger → See all chat messages in that session
2. Click on chat message → See trace timeline with tool executions
3. Correlate multi-agent workflows with conversation context
4. Track token usage and costs alongside execution traces

---

## Code References

### Hook System
- `~/.claude/hooks/stop.py:183-203` - Chat log creation
- `~/.claude/hooks/subagent_stop.py:115-136` - Subagent chat logs
- `~/.claude/hooks/session_start.py:24-46` - Session event logging
- `~/.claude/hooks/post_tool_use.py:46-77` - Tool usage logging
- `~/.claude/hooks/pre_tool_use.py:107-127` - Pre-tool logging

### Configuration
- `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/claude-code-4.5/settings.json:22-88` - Hook definitions
- `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/workflows/multi-agent/orchestration/state/config.json` - Monitoring config

### Logging Infrastructure
- `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/workflows/multi-agent/utils/orchestrator-runner.sh:50-70` - Bash logging functions
- `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/skills/webapp-testing/bin/browser-tools.ts:436-451` - TypeScript console logging

### Log Outputs
- `logs/chat.json` - Chat transcript (requires `--chat` flag)
- `logs/session_start.json` - Session initialization events
- `logs/user_prompt_submit.json` - User prompts
- `logs/pre_tool_use.json` - Pre-tool execution
- `logs/post_tool_use.json` - Post-tool execution
- `logs/subagent_stop.json` - Subagent completion
- `logs/stop.json` - Session stop events
- `/tmp/claude_supervisor_{hash}.log` - Supervisor telemetry

---

## Architecture Insights

### Pattern: Event-Driven Telemetry via Hooks

Claude Code uses a **hook-based event system** where Python scripts are triggered at specific lifecycle events. This pattern is:

- **Extensible**: Add new hooks without modifying core code
- **Configurable**: Enable/disable hooks in `settings.json`
- **Language-agnostic**: Hooks can be Python, JavaScript, or Bash
- **Append-only**: Logs are progressive JSON arrays

### Convention: Progressive JSON Array Logging

All hooks follow this pattern:
1. Read existing JSON array from file (or initialize `[]`)
2. Append new event object
3. Write back to file

This enables:
- Historical event tracking across sessions
- Simple log rotation (archive old files)
- Easy parsing with `jq` or OTEL filelog receiver

### Design Decision: Optional Chat Logging

Chat logs are **NOT created by default** - they require the `--chat` flag. This was likely chosen to:
- Reduce disk usage for non-debugging sessions
- Respect user privacy (transcripts contain all conversation data)
- Allow opt-in telemetry collection

---

## Recommendations

### Immediate Actions

1. **Enable OTEL SDK in Hooks**
   - Install `opentelemetry-api` and `opentelemetry-sdk` in hook environment
   - Add trace context to all log writes
   - Export spans to OTLP endpoint

2. **Deploy OTEL Collector**
   - Use Docker Compose for easy local setup
   - Configure filelog receiver for `logs/*.json`
   - Add transform processor to enrich with resource attributes

3. **Start Jaeger Backend**
   - Run Jaeger all-in-one container
   - Configure OTLP receiver on port 4317
   - Access UI at http://localhost:16686

4. **Add Correlation IDs**
   - Generate `session_id` as root `trace_id`
   - Use tool invocations as child spans
   - Propagate context across subagent spawns

### Future Enhancements

1. **Metrics Collection**
   - Export token usage as OTEL metrics
   - Track cost per session/agent/wave
   - Monitor tool execution latency

2. **Distributed Tracing Across Agents**
   - Propagate trace context when spawning subagents
   - Use W3C Trace Context headers for git worktree operations
   - Create flame graphs for multi-agent workflows

3. **Log Sampling**
   - Implement probabilistic sampling for high-volume sessions
   - Preserve all ERROR/FATAL logs
   - Sample INFO/DEBUG based on session health

4. **Custom OTEL Processor**
   - Create processor to derive spans from chat logs
   - Automatically detect conversation phases (research, planning, implementation)
   - Extract entities (file paths, git commits) as span attributes

---

## Open Questions

1. **Trace Context Propagation**: How should trace context be propagated when spawning subagents in separate tmux sessions?

   **Options**:
   - Environment variables: `TRACEPARENT`, `TRACESTATE`
   - Metadata files: `.otel-trace-context.json` in worktree
   - Command-line arguments: Pass trace context as CLI flags

2. **Session Lifecycle Mapping**: Should each Claude Code session be a single trace, or should user prompts create separate traces?

   **Recommendation**: Session = Root Span, User Prompts = Child Spans, Tool Executions = Grandchild Spans

3. **Cost Attribution**: How to attribute OTEL processing costs to the original session?

   **Solution**: Add `session_id` as resource attribute in Collector pipeline

4. **Backward Compatibility**: Should existing log files be retroactively enriched with trace context?

   **Recommendation**: No - only future logs. Existing logs can be imported as "untraced" events.

---

## References

### Internal Documentation
- Project README: `/Users/stevengonsalvez/d/git/agents-in-a-box/README.md`
- Toolkit README: `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/README.md`
- Multi-Agent Workflow: `/Users/stevengonsalvez/d/git/agents-in-a-box/toolkit/packages/workflows/multi-agent/WORKFLOW.md`

### OpenTelemetry Official Documentation
- [OpenTelemetry Logging Specification](https://opentelemetry.io/docs/specs/otel/logs/)
- [Trace Context in Non-OTLP Log Formats](https://opentelemetry.io/docs/reference/specification/compatibility/logging_trace_context/)
- [Logs Bridge API Specification](https://opentelemetry.io/docs/reference/specification/logs/bridge-api/)
- [Context Propagation](https://opentelemetry.io/docs/concepts/context-propagation/)
- [Logs SDK Specification](https://opentelemetry.io/docs/specs/otel/logs/sdk/)
- [OTLP File Exporter](https://opentelemetry.io/docs/specs/otel/protocol/file-exporter/)
- [Transforming Telemetry](https://opentelemetry.io/docs/collector/transforming-telemetry/)

### OTEL Collector Components
- [Filelog Receiver](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/receiver/filelogreceiver/README.md)
- [Transform Processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/processor/transformprocessor/README.md)

### Best Practices and Guides
- [OpenTelemetry Logs Complete Guide - Uptrace](https://uptrace.dev/opentelemetry/logs)
- [A Complete Guide to OpenTelemetry Logs - Dash0](https://www.dash0.com/knowledge/opentelemetry-logging-explained)
- [Essential OpenTelemetry Best Practices - Better Stack](https://betterstack.com/community/guides/observability/opentelemetry-best-practices/)
- [Parsing Logs with OTEL Collector - SigNoz](https://signoz.io/blog/parsing-logs-with-the-opentelemetry-collector/)
- [Ingesting JSON Logs from Containers - Honeycomb](https://www.honeycomb.io/blog/ingesting-json-logs-containers-otel-collector)

### Language-Specific Integrations
- [Log Correlation - .NET](https://opentelemetry.io/docs/languages/dotnet/logs/correlation/)
- [Logback Appender - OpenTelemetry Java](https://github.com/open-telemetry/opentelemetry-java-instrumentation/blob/main/instrumentation/logback/logback-appender-1.0/library/README.md)

### Vendor-Specific Guides
- [Correlating OTEL Traces and Logs - Datadog](https://docs.datadoghq.com/tracing/other_telemetry/connect_logs_and_traces/opentelemetry/)
- [Log Enrichment with OTEL Collector - New Relic](https://newrelic.com/blog/how-to-relic/enrich-logs-with-opentelemetry-collector)

---

## Summary

**Claude Code HAS native OpenTelemetry support** that can be enabled via environment variables. It exports metrics (token usage, costs, performance) and logs (user prompts, tool results, API requests) via OTLP to backends like Grafana Cloud, Jaeger, Datadog, and SigNoz.

**PLUS Custom Hooks**: Claude Code also uses a **custom hook-based telemetry system** that writes detailed JSON logs to `logs/*.json` files. Chat logs are optionally created via the `--chat` flag in `stop.py` and `subagent_stop.py` hooks.

**Integration Strategy** - Combining Both:

1. **Enable native OTEL** - Set environment variables to export metrics/logs to your backend
2. **Enrich hook logs with trace context** - Add `trace_id`, `span_id`, `trace_flags` to hook outputs
3. **Deploy OTEL Collector** - Use filelog receiver to tail and parse hook JSON logs
4. **Unified observability** - Correlate native OTEL telemetry with detailed chat conversations

**Benefits**:
- **Native telemetry**: Automatic metrics and logs from Claude Code core
- **Custom detail**: Rich conversation transcripts and session lifecycle events
- **Bidirectional correlation**: Navigate from traces to chat logs and vice versa
- **Complete visibility**: See both "what happened" (metrics) and "why" (chat context)

**Next Steps**:
1. Enable native OTEL via environment variables
2. Start using Claude Code - telemetry exports automatically
3. Optionally implement trace context injection in hooks for correlation
4. Deploy OTEL Collector to unify native telemetry + custom chat logs
5. Visualize in Jaeger/Grafana/Datadog
