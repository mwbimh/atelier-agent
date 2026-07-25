# Local Diagnostics

Atelier does not contain a remote telemetry, analytics, crash-reporting, or
trace-upload sink. Logs, metrics, request traces, and debug snapshots remain
on the local machine unless the user explicitly sends them elsewhere.

## Debug Logs

Write one log file:

```powershell
ate --debug-file C:\tmp\atelier-debug.log
```

Write per-session logs under `~/.atelier/debug/`:

```powershell
ate --debug
```

Equivalent environment variables:

| Variable | Meaning |
|---|---|
| `ATELIER_DEBUG_LOG=1` | Write per-session files under `~/.atelier/debug/` |
| `ATELIER_DEBUG_LOG=<path>` | Write one local file |
| `ATELIER_LOG_FILE=<path>` | Write one local file |
| `RUST_LOG=<filter>` | Select the local log level and targets |

Debug files can contain prompts, tool arguments, paths, and command output.
Treat them as sensitive local artifacts.
