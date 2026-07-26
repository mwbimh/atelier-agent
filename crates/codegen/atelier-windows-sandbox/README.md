# Atelier Windows sandbox

This crate is the local Windows implementation based on the pinned
`codex-rs/windows-sandbox-rs` source at commit
`71448a29e7343b9613eaea620fcdbd196aed2af0`.

The active process chain is:

- On the first interactive launch, Atelier asks whether to enable the Windows
  sandbox. Accepting launches the hidden elevated setup mode and creates
  `AtelierSandbox` for network-allowed processes and `AtelierSandboxNoNet` for
  network-disabled processes. Their random passwords are protected with
  machine-scope DPAPI under `~/.atelier/.sandbox-secrets`. The explicit
  `ate sandbox setup` command remains available for automation and repair.
- Persistent WFP rules at `ALE_AUTH_CONNECT_V4/V6` block outbound TCP and UDP
  for the `AtelierSandboxNoNet` account SID. Setup and status verify the rule
  shape and SID binding; WFP failure leaves setup unavailable (fail-closed).
- The current `ate.exe` is materialized under `~/.atelier/.sandbox-bin`; the
  release directory contains the public `ate.exe` and the offline
  `install-windows.ps1` installer, but no standalone helper executable.
- The parent starts the materialized binary with `CreateProcessWithLogonW` and
  exchanges the spawn request and raw standard streams through sandbox-user
  scoped named pipes.
- The persistent-user runner derives a user-aware `WRITE_RESTRICTED` token,
  starts the target with `CreateProcessAsUserW`, and remains in a parent-owned
  kill-on-close Job Object.
- Workspace roots receive temporary account and capability ACL entries. The
  original ACLs are restored when the sandboxed process exits.
- `ATELIER_HOME` is the only home-directory variable introduced by this
  crate. Telemetry is `None`/no-op and OTEL exporter variables are disabled in
  the child environment.

The following upstream capability is not implemented here:

- ConPTY/TTY session support.

Those features require the larger product dependency graph and separate
security review. This crate fails closed when roots, cwd, or the command path
cannot be normalized, and it never falls back to an unsandboxed child.

## Internal release entry points

The single public binary hosts all helpers behind hidden markers:

```text
ate.exe --internal-windows-sandbox-setup <payload>
ate.exe --internal-windows-sandbox-runner <pipe arguments>
ate.exe --internal-command-runner ...
ate.exe --internal-workspace-worker --root <workspace-root>
```

These markers are handled before TUI, configuration, logging, or telemetry
startup and are intentionally omitted from `ate --help`.

## Public management commands

```text
ate sandbox setup
ate sandbox status
ate sandbox status --json
ate sandbox reset
ate sandbox reset --yes
```

- `setup` explicitly opens one UAC prompt and also restores the native
  `workspace` sandbox preference when it had previously been declined.
- Interactive startup invokes the same setup path after the user accepts the
  first-launch sandbox question; no manual command is required.
- `status` is read-only and never elevates.
- `reset` removes the WFP provider/sublayer/filters, both sandbox accounts,
  setup marker, and DPAPI credential file. Without `--yes`, the user must type
  `reset` exactly before the UAC helper is launched.

The WFP implementation is derived from the pinned Apache-2.0 Codex source
listed above. Atelier uses its own stable WFP GUID namespace and removes the
Codex telemetry path.

After one-time setup, run the real OS-boundary test with:

```powershell
cargo test --locked -p atelier-windows-sandbox --test network_wfp_e2e -- --ignored --nocapture --test-threads=1
```
