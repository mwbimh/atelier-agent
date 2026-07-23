# Atelier Workspace Worker

The Workspace Worker is a local process boundary for workspace file
operations. Release packages expose only the main `atelier` executable. The
Worker implementation is embedded in that executable and is started
automatically in a hidden internal mode.

Users do not need either of these files beside a release build:

```text
atelier-workspace-worker.exe
atelier-command-runner.exe
```

On Windows, a normal release directory can contain only:

```text
atelier.exe
```

The process boundary still exists. Atelier starts another instance of the main
executable with an internal argument so a Worker failure does not silently turn
into unrestricted host-side file access.

## Normal Startup

Run Atelier from the workspace directory:

```bash
cd /path/to/project
atelier
```

Or select another workspace explicitly:

```bash
atelier --cwd /path/to/project
```

The Runtime creates a Worker bound to the canonical workspace root. Separate
workspace roots receive separate Worker Runtime instances.

## Internal Modes

Packaged builds start embedded helpers automatically:

```text
atelier.exe --internal-workspace-worker --root <workspace-root>
atelier.exe --internal-command-runner ...
```

These modes are implementation details, not user commands. Do not launch them
manually during normal use.

For development and tests only, the Runtime still accepts explicit helper
overrides through `ATELIER_WORKSPACE_WORKER` and `ATELIER_COMMAND_RUNNER`.
Those overrides are not required by a release installation.

## Worker Protocol

The Worker uses newline-delimited JSON over the child process's stdin/stdout.
`WORKER_PROTOCOL_VERSION` is currently `1`, and each frame is limited to
`8 MiB` including the newline.

The handshake binds the Worker to one canonical root:

```text
client -> hello(protocol_version, nonce, workspace_root)
worker -> ready(protocol_version, workspace_root)
```

Each request includes the protocol version, nonce, and a monotonically
increasing request ID. The Worker rejects mismatched roots, protocol versions,
nonces, response IDs, and methods outside its namespace.

Binary file methods use base64 data:

```text
atelier.worker.read_file
atelier.worker.write_file
atelier.worker.delete_file
```

Path resolution is confined to the bound workspace root, including supported
reparse-point and symlink escape checks.

## Failure and Shutdown

EOF, malformed frames, identity mismatches, protocol mismatches, and Worker
exit are surfaced as workspace errors. The caller does not silently fall back
to unrestricted local filesystem access.

Normal shutdown uses a `shutdown` frame and matching `bye` response. The child
also uses process cleanup on drop as a last resort.

## Current Isolation Boundary

The embedded packaging change does not by itself make every workspace action
fully sandboxed. Diagnostics report the effective backend. The current preview
Worker boundary covers the integrated workspace file path; some Git, search,
patch, terminal, PTY, checkpoint, and hunk-tracker paths still use their
existing Runtime implementations.

The full release gate remains routing all workspace-affecting operations
through an isolated Worker with cancellation, streaming/backpressure, and
per-session lifecycle management.

## Security Invariants

1. The Worker root is fixed during the handshake.
2. A Worker crash is reported and never triggers an unsandboxed retry.
3. Release packaging resolves the embedded helper through the running
   `atelier` executable.
4. Preview path confinement is not described as complete OS sandboxing.
5. Protocol output remains machine-readable; diagnostics go to stderr.
