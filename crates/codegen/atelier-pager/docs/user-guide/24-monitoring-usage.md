# Optional OpenTelemetry Export

> **Status: alpha.** The exported schema is versioned with
> `atelier_code.schema.version = v1`.

Atelier can export usage metrics and events to an OpenTelemetry collector that
you configure. This is optional and off by default. Atelier does not send
product analytics, session traces, crash reports, or usage events to a built-in
collector.

The optional stream requires both a master opt-in and an exporter selection.
It is content-free by default: prompts, source code, file paths, tool arguments,
and shell commands are not exported unless an explicitly documented content
gate permits them. Shell command text is never exported by the v1 schema.

## Quick Start

```bash
export ATELIER_EXTERNAL_OTEL=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_EXPORTER_OTLP_ENDPOINT=https://collector.corp.example:4318
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <collector-token>"
atelier
```

`ATELIER_EXTERNAL_OTEL=1` alone exports nothing. At least one exporter must
also be selected. Conversely, `OTEL_*` exporter variables do nothing without
the Atelier master switch.

## Environment Variables

| Variable | Default | Meaning |
|---|---|---|
| `ATELIER_EXTERNAL_OTEL` | `0` | Master switch |
| `OTEL_METRICS_EXPORTER` | `none` | `otlp`, `console`, or `none` |
| `OTEL_LOGS_EXPORTER` | `none` | `otlp`, `console`, or `none` |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` | `http/protobuf` or `grpc` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | local collector default | Base collector endpoint |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | unset | Signal-specific logs endpoint |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | unset | Signal-specific metrics endpoint |
| `OTEL_EXPORTER_OTLP_HEADERS` | unset | Collector authentication headers; not stored on disk |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `10000` | Export timeout in milliseconds |
| `OTEL_METRIC_EXPORT_INTERVAL` | `60000` | Metric export interval in milliseconds |
| `OTEL_BLRP_SCHEDULE_DELAY` | `5000` | Event batch interval in milliseconds |
| `OTEL_METRICS_INCLUDE_SESSION_ID` | `1` | Include session ID on metrics |
| `OTEL_METRICS_INCLUDE_VERSION` | `0` | Include application version |
| `OTEL_LOG_USER_PROMPTS` | `0` | Export scrubbed prompt text, capped at 60 KiB |
| `OTEL_LOG_TOOL_DETAILS` | `0` | Export capped tool details and file paths |

`OTEL_RESOURCE_ATTRIBUTES` is ignored; Atelier builds the resource from a
fixed attribute set. Supply collector credentials only through the standard
OTEL header environment variables.

## Config File

The same opt-in can be stored in the local `$ATELIER_HOME/config.toml`:

```toml
[telemetry]
otel_enabled = true
otel_metrics_exporter = "otlp"
otel_logs_exporter = "otlp"
otel_endpoint = "https://collector.corp.example:4318"
otel_protocol = "http/protobuf"
otel_log_user_prompts = false
otel_log_tool_details = false
```

Environment variables take precedence. There is deliberately no config-file
headers key, so collector tokens do not need to be written to disk.

## Exported Metrics

| Metric | Unit | Important attributes |
|---|---|---|
| `atelier_code.session.count` | session | entrypoint and terminal metadata |
| `atelier_code.token.usage` | token | token type and model |
| `atelier_code.turn.count` | turn | outcome and model |
| `atelier_code.tool.decision` | decision | tool, decision, access kind, permission mode |
| `atelier_code.tool.usage` | call | tool and outcome |
| `atelier_code.error.count` | error | error category and model |

There is no cost metric. Join token usage with your own model price data.

## Exported Events

The v1 event set includes session start/end, user prompt metadata, turn
completion, Provider requests and errors, tool results and decisions, MCP
connections, permission mode changes, skill/plugin activation, compaction,
subagents, internal error classes, and model switches.

Prompt text requires `OTEL_LOG_USER_PROMPTS=1`. Tool parameters, full file
paths, and verbatim MCP/skill/plugin names require
`OTEL_LOG_TOOL_DETAILS=1`. Event fields remain size-capped and scrubbed.

## Privacy Model

The exporter applies three fail-closed layers:

1. a closed typed schema for attribute keys;
2. emit-time secret and home-directory scrubbing with size limits;
3. export-time validation that drops records with unknown or gated fields.

Never exported by v1: shell command text, full error messages, API keys,
Authorization headers, cookies, machine fingerprints, or Provider credentials.

Cloud model Providers and user-configured MCP servers have their own data
handling policies. OpenTelemetry export settings do not control data sent to
those explicitly configured endpoints.

## Debugging

Use console exporters to inspect redacted records locally:

```bash
ATELIER_EXTERNAL_OTEL=1 \
OTEL_LOGS_EXPORTER=console \
OTEL_METRICS_EXPORTER=console \
atelier
```

Exporter diagnostics are written to stderr/local logs. Export failures do not
fall back to another endpoint.
