# Getting Started

Atelier is a local-first terminal coding agent. It runs as a TUI (Terminal User Interface) that understands your codebase, executes shell commands, edits files, searches the web when requested, and manages tasks.

You can use it interactively as a full-screen TUI, run it headlessly for scripting and CI/CD, or integrate it into editors via the Agent Client Protocol (ACP).

---

## Installation

Install the packaged release with npm:

```bash
npm install -g @atelier/atelier
```

Or build the release binary from source:

```bash
cargo build -p atelier-pager-bin --bin atelier --release
```

The user-facing release contains one executable. On Windows this is
`atelier.exe`; the Workspace Worker and command runner are embedded and start
as hidden child-process modes of the same executable.

The npm package installs the executable under `~/.atelier/bin`
(`%USERPROFILE%\.atelier\bin` on Windows). `ATELIER_HOME` controls Runtime
state after launch; it does not relocate the npm-installed executable.

Verify the installation:

```bash
atelier --version
```

Update npm installations through npm:

```bash
npm install -g @atelier/atelier@latest
```

---

## First Launch

Start Atelier by running:

```bash
atelier
```

Atelier does not include a hosted default model or product login. Configure a
Provider and assign the required Roles before sending a prompt.

Set the Provider credential in the environment, then start the TUI:

```bash
export ALLM_API_KEY="..."
atelier
```

Inside Atelier, run either the interactive commands or their complete forms:

```text
/provider add allm chat https://api.example.com/v1 env:ALLM_API_KEY
/provider test allm
/provider refresh allm
/model
/roles
```

The current directory becomes the workspace. To use another directory, launch
with `atelier --cwd <path>`.

See [Provider Credentials](02-authentication.md) and
[Providers, Models, and Roles](11-custom-models.md).

---

## Basic Interaction

Once configured, Atelier presents a full-screen TUI with two main areas:

- **Scrollback** -- the conversation history showing your prompts, Atelier's responses, tool calls, file edits, and more.
- **Prompt** -- the input area at the bottom where you type messages.

Type a message and press `Enter` to send it. Atelier reads files, runs commands, and edits code as needed. Each tool run streams into the scrollback in real time.

Press `Tab` to move focus between the prompt and the scrollback. While a turn is running, `Ctrl+C` cancels it (or clears a non-empty draft first); `Esc` is a no-op mid-turn. Idle, press `Esc` twice within 800ms to clear a non-empty prompt, or (with an empty prompt and conversation messages) to open rewind — see [Keyboard Shortcuts](03-keyboard-shortcuts.md#escape). With the scrollback focused, use the arrow keys to select entries and to collapse or expand them. To navigate with `j`/`k` and fold with `h`/`l` instead, enable Vim mode.

### File References

Use `@` in your prompt to attach files:

```
@src/main.rs              # Attach a file
@src/main.rs:10-50        # Attach lines 10-50
@src/                     # Browse a directory
```

The `@` operator opens a fuzzy file picker. By default it respects `.gitignore` and hides dotfiles. Prefix with `!` to search hidden files:

```
@!.github                 # Search hidden files
@!.env                    # Attach a .env file
```

### Permissions

By default, Atelier asks for permission before executing shell commands or editing files. You can approve individually or toggle always-approve mode:

- Press `Ctrl+O` to toggle always-approve mode
- Use the `--yolo` flag at launch: `atelier --yolo`
- Type `/always-approve` in the prompt to toggle the mode

---

## Key Concepts

### Sessions

Every conversation is a **session**. Sessions are automatically saved to `~/.atelier/sessions/` and can be resumed later. Each session tracks the full conversation history, tool calls, file edits, and task state.

- Start a new session: `Ctrl+N` or `/new`
- Resume a previous session: `/resume` in the TUI, or `--resume <ID>` from the CLI
- Continue the most recent session: `atelier -c`

### Scrollback

The scrollback is the main display area. It shows:

- **User prompts** -- your messages, rendered as sticky headers
- **Agent messages** -- Atelier's responses with full markdown rendering and syntax highlighting
- **Thinking blocks** -- Atelier's reasoning process (collapsible)
- **Tool calls** -- file edits (with inline diffs), command executions, search results, and more
- **Task lists** -- TODO items tracking progress

Collapse or expand the selected entry with the `Left`/`Right` arrow keys (or `h`/`l` and `e` in Vim mode). In Vim mode, press `y` to copy its content and `Y` to copy its metadata (for example, the command that ran). Press `Enter` to open it in the fullscreen viewer (in any mode).

### Tools

Atelier has built-in tools for:

| Tool | Description |
|------|-------------|
| `read_file` / `search_replace` | Read and edit files with line-precise changes |
| `grep` | Regex search across your codebase (powered by ripgrep) |
| `list_dir` | List directory contents |
| `run_terminal_command` | Execute shell commands |
| `web_search` / `web_fetch` | Search the web and fetch URLs |
| `todo_write` | Create and manage task lists |
| `spawn_subagent` | Spawn parallel subagent sessions |
| `memory_search` | Search cross-session memory |

Tools can be extended with [MCP servers](05-configuration.md#mcp-servers) for integrations like GitHub, databases, and more.

### Slash Commands

Type `/` in the prompt to access commands. These provide quick actions without writing a full prompt:

```
/model allm/deepseek-v4-flash       # Switch model
/provider                           # Manage Providers interactively
/roles                              # Configure fixed Runtime Roles
/compact                          # Compress conversation history
/always-approve                   # Toggle always-approve mode
/new                              # Start a new session
```

See [Slash Commands](04-slash-commands.md) for the complete reference.

---

## Common Launch Options

```bash
# Launch the interactive TUI and submit an initial prompt as the first turn
atelier "fix the failing auth test and run it"

# Initial prompt in a new git worktree. Use --worktree=<name> (with `=`) so the
# prompt isn't swallowed as the worktree name — `atelier -w "refactor module X"`
# would treat "refactor module X" as the worktree label, not the prompt.
atelier --worktree=feat "refactor module X"

# Base the worktree on a specific branch (e.g. main) instead of the current HEAD:
atelier -w --ref main "implement feature from main"


# Start in a specific project directory
atelier --cwd ~/projects/my-app

# Add project-specific rules
atelier --rules "Always use TypeScript. Prefer functional components."

# Auto-approve all tool executions
atelier --yolo

# Use a specific model
atelier -m allm/deepseek-v4-flash

# Resume a previous session
atelier --resume <session-id>

# Continue the most recent session
atelier -c

# Experimental scrollback-native render mode. Sticky: plain `atelier` reopens in
# the mode last chosen via --minimal/--fullscreen (or /minimal//fullscreen).
atelier --minimal

# Back to the standard fullscreen TUI (and make it sticky again)
atelier --fullscreen

# Headless mode (for scripts)
atelier -p "Explain this codebase"
```

---

## Headless Mode

Run Atelier non-interactively for scripting, CI/CD, and automation:

```bash
atelier -p "Your prompt here"
```

Output formats:

| Format | Flag | Description |
|--------|------|-------------|
| `plain` | (default) | Human-readable text |
| `json` | `--output-format json` | Single JSON object with `text`, `stopReason`, `sessionId`, and `requestId` |
| `streaming-json` | `--output-format streaming-json` | NDJSON event stream for real-time processing |

Example CI/CD usage:

```bash
atelier -p "Review changes for bugs" --output-format json --yolo | jq -r '.text'
```

---

## Project Rules (AGENTS.md)

Add per-project instructions by creating an `AGENTS.md` file in your repository. Atelier reads these files and injects their contents as a project-instructions message at the start of the conversation:

```
$ATELIER_HOME/AGENTS.md        # Global rules (default: ~/.atelier/AGENTS.md)
<repo-root>/AGENTS.md       # Repository-level rules
<cwd>/AGENTS.md             # Directory-level rules (highest priority)
```

Deeper files take precedence. Atelier also reads `CLAUDE.md` files for compatibility.

---

## Where to Go Next

| Document | What You Will Learn |
|----------|-------------------|
| [Provider Credentials](02-authentication.md) | Provider CRUD, credentials, discovery, and local storage |
| [Keyboard Shortcuts](03-keyboard-shortcuts.md) | Complete reference for all key bindings |
| [Slash Commands](04-slash-commands.md) | All available `/` commands |
| [Configuration](05-configuration.md) | `ATELIER_HOME`, config.toml, pager.toml, and environment variables |
| [Providers, Models, and Roles](11-custom-models.md) | Model discovery, selection, Wire API, and eight fixed Roles |
