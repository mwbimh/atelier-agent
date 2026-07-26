# Atelier Shell Runtime

`atelier-shell` contains Atelier's session actor, Agent loop, persistence,
compaction, Goal orchestration, subagents, hooks, policy checks and ACP runtime.
The public executable is `ate.exe`; this crate is not a standalone CLI.

## Privacy model

Atelier has no built-in remote telemetry, crash upload, trace donation, remote
settings or vendor authentication. Local logs, metrics and traces may be written
under `~/.atelier/logs/` when enabled.

Network access is limited to capabilities explicitly configured by the user:

- model Provider inference and model discovery;
- Provider OAuth endpoints;
- user-configured MCP servers;
- explicit web fetch/search tools;
- experimental Provider/model endpoints such as remote compaction or image
  generation.

There is no global API key or vendor endpoint fallback. Every model request is
routed through a `provider/model` selection.

## Configuration

The user configuration root is `~/.atelier/`:

```text
~/.atelier/
├─ config.toml
├─ providers.toml
├─ roles.toml
├─ request-agents.toml
├─ credentials/oauth/
├─ models/default/models.toml
├─ models/providers/<provider>/models.toml
├─ contexts/<preset>/
├─ branding/logo.txt
├─ cache/models/
├─ logs/
└─ sessions/
```

`config.toml` selects the context preset and request-agent identity. It does not
select a model on first run: users must configure a Provider and choose their
own default model. Provider connection and authentication settings live in
`providers.toml`. Model-specific `wire_api`, context window, effort levels,
fast-mode support, payload and experimental endpoints live under `models/`.

First run creates the built-in files without a default model. `/settings
reset-defaults` restores only `models/default/` and `contexts/default/`; it does
not change `config.toml`, Providers, Roles, request agents, branding, or other
user settings. In the TUI, use:

```text
/settings reset-defaults
/provider
/model
/effort
/fast-mode
/wire-api
/roles
```

## Provider OAuth

Providers may declare authorization-code with PKCE and/or device-code flows.
Credentials are stored in the operating-system secret store; configuration
files contain only the Provider OAuth method metadata.

Example:

```text
/provider add company https://api.example.com/v1 bearer oauth authorization-code desktop-client https://login.example.com/authorize https://login.example.com/token openid,profile
/provider login company authorization-code
```

## Goal roles

Goal orchestration uses the fixed `planner`, `strategist` and `skeptic` roles.
Each role resolves its configured Provider/model independently and fails open to
the current session model only when the configured model is unavailable,
unauthorized or lacks the required tool capability.

## Windows sandbox

Run the one-time setup from a normal PowerShell window and approve UAC:

```powershell
ate sandbox setup
ate sandbox status --json
```

The command runner and Workspace Worker are hidden modes embedded in `ate.exe`;
they are not separate release binaries.

## Build

```powershell
cargo check --locked -p atelier-shell
cargo test --locked -p atelier-shell --lib
.\tools\build-release.ps1 -CleanOutput
```

The repository-level [README](../../../README.md) and the
[user guide](../atelier-pager/docs/user-guide/) contain end-user instructions.
