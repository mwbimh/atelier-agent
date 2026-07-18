# Atelier Workspace Worker

`atelier-workspace-worker` is the local process boundary for workspace file
operations. It is intentionally a separate child process from the Agent so a
Worker failure cannot silently turn into host-side file access. In release
artifacts, the Worker implementation is embedded in `atelier.exe`; the child
still runs as a separate process, but the package no longer needs to ship a
second `atelier-workspace-worker.exe` beside it.

## Binary and startup

The standalone binary is built from the `atelier-workspace` crate:

```text
cargo build -p atelier-workspace --bin atelier-workspace-worker
```

Packaged release builds normally use the main executable in hidden internal
mode:

```text
atelier.exe --internal-workspace-worker --root <workspace-root>
```

The runtime selects this mode automatically when `current_exe()` is
`atelier.exe`. `ATELIER_WORKSPACE_WORKER` remains an explicit override for
development, tests, or a package that intentionally ships the standalone
worker.

The Worker accepts one required argument:

```text
atelier-workspace-worker --root <workspace-root>
```

The standalone binary and the hidden `atelier.exe` mode speak the same
protocol and accept the same `--root` argument.

On Windows, the client starts it through the Codex-derived command runner when
the native sandbox is active. An explicit
`ATELIER_SANDBOX_BACKEND=unsafe` is the only mode that permits direct
unsandboxed startup. A missing helper or an unavailable native sandbox returns
an error before the Worker is started.

## Protocol

The transport is newline-delimited JSON (NDJSON) over the child process's
stdin/stdout. `WORKER_PROTOCOL_VERSION` is currently `1`, and each frame is
limited to `8 MiB` including the newline.

The handshake is:

```text
client → hello(protocol_version, nonce, workspace_root)
worker → ready(protocol_version, workspace_root)
```

Every call repeats the protocol version and nonce and contains a monotonically
increasing request ID. The Worker canonicalizes its startup root and rejects a
hello whose canonical root differs from it. It also rejects methods outside
the `workspace.*` and `atelier.worker.*` namespaces.

The current binary-safe file methods are:

```text
atelier.worker.read_file
atelier.worker.write_file
atelier.worker.delete_file
```

File bytes are base64-encoded. All paths are resolved through the Worker's
workspace-root confinement checks, including reparse/symlink escape checks
where the platform supports them.

## Failure and shutdown

The client serializes calls on one connection and treats EOF, a malformed
frame, a mismatched response ID, a protocol mismatch, a nonce mismatch, or a
Worker exit as an error. The caller never falls back to `LocalFs` after one of
these failures.

Normal shutdown uses a `shutdown` frame and expects a matching `bye` response.
The child process also has `kill_on_drop` enabled as a last-resort cleanup;
runtime owners should call `WorkspaceWorkerClient::shutdown` when they have a
graceful shutdown path.

## Current integration boundary

The first integration pass installs `WorkspaceWorkerFs` in the production
`WorkspaceSessionContextFactory` used by `connect_local_workspace`. This
routes the built-in binary file read/write/delete interface through the Worker.

The following paths still use the existing host-side or sandbox-preview
implementations and are not yet the full Worker boundary:

- `WorkspaceOps` local-mode Git and filesystem RPCs;
- search and patch helpers that access the workspace directly;
- the terminal, background task, and PTY backend;
- some session checkpoint and hunk-tracker filesystem paths.

Therefore the current status is `sandbox-preview`, not `full`. The full
release gate requires routing every file, search, patch, Git, shell, and PTY
operation through a Worker with cancellation, streaming/backpressure, and
per-session lifecycle management.

The embedded mode only changes how the Worker code is packaged. It does not
remove the process boundary or expand the current `sandbox-preview` scope.

## Security invariants

1. The Worker root is bound during the handshake and cannot be changed by a
   later request.
2. A Worker crash is surfaced as a workspace error and never triggers an
   unsandboxed retry.
3. A Worker binary that is absent is a startup error for production workspace
   construction.
4. The `unsafe` backend is opt-in and must remain visible in diagnostics.
5. The NDJSON stream is machine-readable; diagnostics go to stderr.
