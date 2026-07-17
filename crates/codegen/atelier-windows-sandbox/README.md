# Atelier Windows sandbox

This crate is the first local Windows import based on the pinned
`codex-rs/windows-sandbox-rs` source at commit
`71448a29e7343b9613eaea620fcdbd196aed2af0`.

The active surface is intentionally small and real:

- `run_command` validates existing roots and the command cwd, creates a
  restricted primary token, grants a temporary capability SID through ACLs,
  starts the child with `CreateProcessAsUserW`, captures stdout/stderr, and
  restores the original ACLs.
- `atelier-command-runner` exposes the same operation as a fail-closed helper
  CLI.
- `ATELIER_HOME` is the only home-directory variable introduced by this
  crate. Telemetry is `None`/no-op and OTEL exporter variables are disabled in
  the child environment.

The following upstream capabilities are deliberately not active in this
first-stage import and must not be inferred from this crate:

- elevated provisioning and its IPC protocol;
- Windows Filtering Platform (WFP) network enforcement;
- ConPTY/TTY session support.

Those features require the larger product dependency graph and separate
security review. This crate fails closed when roots, cwd, or the command path
cannot be normalized, and it never falls back to an unsandboxed child.
