# Atelier Workspace Worker

The Workspace Worker is a local process boundary for workspace operations.
Release packages expose only the public `ate` executable. Worker and command
runner implementations are embedded hidden modes of that executable, so users
do not install separate helper binaries.

A Windows release contains:

```text
ate.exe
install-windows.ps1
```

It does not contain:

```text
atelier-workspace-worker.exe
atelier-command-runner.exe
```

## Startup

Run Atelier from a workspace:

```bash
cd /path/to/project
ate
```

Or select one explicitly:

```bash
ate --cwd /path/to/project
```

The runtime creates a Worker bound to the canonical workspace root. Different
roots receive different Worker instances.

Internal process modes are implementation details:

```text
ate.exe --internal-workspace-worker --root <workspace-root>
ate.exe --internal-command-runner ...
```

Development and test environments may override helper resolution through
`ATELIER_WORKSPACE_WORKER` and `ATELIER_COMMAND_RUNNER`. Normal release
installations do not need these variables.

## Protocol

The Worker uses bounded newline-delimited JSON over its child-process channel.
The handshake binds the protocol version, a nonce, and one canonical workspace
root:

```text
client -> hello(protocol_version, nonce, workspace_root)
worker -> ready(protocol_version, workspace_root)
```

Every request includes the protocol version, nonce, and a monotonically
increasing request ID. The Worker rejects mismatched roots, versions, nonces,
response IDs, and methods outside its namespace.

Binary file methods encode payloads as base64. Path resolution remains inside
the bound root and includes supported reparse-point and symlink escape checks.

## Windows isolation

After explicit UAC setup, Windows release sessions launch Workers under the
persistent restricted sandbox identity. The runtime applies restricted tokens,
workspace ACL capabilities, profile-aware environment construction, Job Object
lifetime control, named-pipe authorization, and account-bound WFP network
rules.

The first launch for a canonical root/access-mode pair installs and propagates a
stable capability ACL. Later launches verify and reuse that ACE. This removes
the previous recursive ACL rewrite from every Session startup while preserving
fail-closed capability isolation. Startup instrumentation records credential,
runner materialization, ACL, logon, pipe, and Worker-handshake timings.

Check readiness with:

```powershell
ate sandbox status --json
```

The runtime fails closed when the required sandbox chain is unavailable; it
does not silently retry the operation as the unrestricted host user.

## Failure and shutdown

EOF, malformed frames, identity mismatches, protocol mismatches, and Worker exit
are surfaced as workspace errors. Normal shutdown uses a `shutdown` frame and a
matching `bye` response. Process cleanup remains a final lifecycle safeguard.

## Security invariants

1. The canonical Worker root is fixed during the handshake.
2. Worker requests are bound to one version, nonce, and request sequence.
3. A Worker or sandbox failure never triggers an unrestricted retry.
4. Release helper modes resolve through the running `ate` executable.
5. Protocol output remains machine-readable; diagnostics go to stderr.
6. Native OS-boundary E2E is required before declaring a platform release
   ready.
