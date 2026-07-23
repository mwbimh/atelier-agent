# Atelier

Atelier is a local-first terminal coding agent with an interactive TUI,
headless mode, ACP integration, sandboxed workspace operations, subagents, and
parallel tasks.

## Install

```bash
npm install -g @atelier/atelier
```

The package installs one user-facing executable:

```text
~/.atelier/bin/atelier        # macOS/Linux
%USERPROFILE%\.atelier\bin\atelier.exe  # Windows
```

Workspace and Windows command helpers are embedded in the main executable.
They still run as isolated child processes, but no additional helper executable
needs to be copied beside `atelier`.

## Configure a Provider

Atelier has no hosted default model and does not use browser login. Configure a
Provider before starting a session:

```bash
export ALLM_API_KEY="..."
atelier
```

Then use the TUI:

```text
/provider add allm chat https://your-provider.example/v1 env:ALLM_API_KEY
/provider test allm
/provider refresh allm
/model
```

`/provider` without arguments opens the interactive command picker. Model
discovery uses the Provider's configured `/models` endpoint. Use `/roles` to
assign Provider/model pairs to `main`, `explore`, `implement`, `review`, `test`,
`compact`, `summary`, and `title`.

See the user guide sections on [Provider credentials](../../docs/user-guide/02-authentication.md)
and [Providers, models, and Roles](../../docs/user-guide/11-custom-models.md).

## Run

```bash
# Use the current directory as the workspace
atelier

# Use another directory as the workspace
atelier --cwd /path/to/project

# Run one headless task
atelier -p "Explain this codebase"
```

## State Directory

Atelier stores local configuration, Provider metadata, sessions, logs, and
other state under `~/.atelier` by default. Set `ATELIER_HOME` before launch to
use another directory:

```bash
export ATELIER_HOME=/path/to/atelier-home
atelier
```

## Update

Update through the package manager:

```bash
npm install -g @atelier/atelier@latest
```

## Supported Platforms

| Platform | Architecture |
|---|---|
| macOS | arm64, x86_64 |
| Linux | arm64, x86_64 |
| Windows | arm64, x86_64 |
