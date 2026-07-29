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
  release directory contains the public `ate.exe` and
  `install-windows.ps1`, but no standalone helper executable. The installer
  can download a verified self-contained PowerShell ZIP or use an explicit
  offline `-PowerShellArchive` together with the required
  `-PowerShellArchiveSha256`.
- The parent starts the materialized binary with `CreateProcessWithLogonW` and
  exchanges the spawn request and raw standard streams through sandbox-user
  scoped named pipes.
- The persistent-user runner derives a user-aware `WRITE_RESTRICTED` token,
  starts the target with `CreateProcessAsUserW`, and remains in a parent-owned
  kill-on-close Job Object.
- Each canonical workspace root and access mode receives a stable capability
  SID stored under `~/.atelier/.sandbox/capabilities`. The first launch for a
  root propagates the account/capability ACL once; later launches verify and
  reuse the exact inheritable ACEs instead of recursively rewriting the whole
  repository. Read-only and workspace-write modes use different capabilities.
- The restricted child requires the workspace capability in its restricting
  SID set. The sandbox account SID remains on the normal-token side of the
  Windows access check, so a broader account ACE cannot bypass a read-only
  capability.
- `ATELIER_HOME` is the only home-directory variable introduced by this
  crate. Telemetry is `None`/no-op and OTEL exporter variables are disabled in
  the child environment.
- Native Windows shell selection is fixed at Session startup: managed portable
  PowerShell 7, then machine-wide PowerShell 7, then Windows PowerShell 5.1,
  otherwise fail closed. WindowsApps/MSIX aliases, Git Bash, cmd.exe, and WSL
  are not automatic shell candidates. Commands always use the resolved
  absolute PowerShell path and are never replayed through another shell after
  a command failure.
- The installer places managed PowerShell under
  `C:\ProgramData\Atelier\runtimes\powershell\<version>` and writes
  `active.json`. Executable roots approved by the installer are recorded in
  `C:\ProgramData\Atelier\tools\registry.json`; the sandbox child receives a
  controlled PATH instead of the host user's complete PATH.

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

After one-time setup, run the real OS-boundary tests with a built single-binary
runner:

```powershell
$env:ATE_BINARY = (Resolve-Path target\debug\ate.exe)
cargo test --locked -p atelier-windows-sandbox --test contract -- --nocapture --test-threads=1
cargo test --locked -p atelier-windows-sandbox --test network_wfp_e2e -- --ignored --nocapture --test-threads=1
```

Process-boundary tests skip with an explicit message when `ATE_BINARY` (or
`ATELIER_SANDBOX_RUNNER`) is absent; they no longer launch the Rust test harness
as if it implemented the hidden runner mode.
