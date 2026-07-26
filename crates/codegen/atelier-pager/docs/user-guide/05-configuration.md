# Configuration

Atelier reads configuration from local config files, environment variables, and
CLI flags. This document covers the common options.

---

## Precedence

Configuration is resolved in this order (highest priority first):

1. **CLI flags** (e.g., `--yolo`, `--model`, `--sandbox`)
2. **Environment variables** (e.g., Provider credential variables, `ATELIER_MEMORY`)
3. **Workspace config** (`<workspace>/.atelier/config.toml` where supported)
4. **User config** (`$ATELIER_HOME/config.toml`)
5. **Built-in defaults**

---

## config.toml (Main Configuration)

Location: `$ATELIER_HOME/config.toml` (default: `~/.atelier/config.toml`)

If the file does not exist, Atelier uses built-in defaults. Specify only the values you want to override.

### Runtime Selection

```toml
model = "provider/model" # optional; omit it until a Provider/model is selected
context = "default"
request_agent = "atelier"
```

`model` is a top-level Provider/model composite key. Atelier does not select the
first discovered model automatically. Context selection and Request Agent
selection are also top-level runtime settings; they do not belong in
`providers.toml`, `roles.toml`, or a `[models]` table.

### General Settings

```toml
[ui]
simple_mode = true                      # readline-style prompt editing (default); false = vim editing in the prompt
vim_mode = false                       # vim-style scrollback navigation keys (default: false)
max_thoughts_width = 120               # max column width for reasoning display
default_selected_permission = "always_allow_all_sessions" # preselected row on the FIRST approval prompt
remember_tool_approvals = false        # show per-command "Always allow" options on permission prompts;
                                       # grants are remembered per project (default: false); see 22-permissions-and-safety.md
show_thinking_blocks = true            # show agent thinking blocks in the TUI (default: true)
group_tool_verbs = true                # fold runs of read/search/list tool calls and subagent rows
                                       # — and finished thoughts among them — into one row (default: true)
collapsed_edit_blocks = false          # show edits as one-line +N/-M diffstat summaries and merge
                                       # back-to-back same-file edits into one row, expand for the
                                       # diffs (default: false; pager.toml [scrollback.blocks.edit]
                                       # expanded_by_default/line_summary override its fold shape)
screen_mode = "fullscreen"             # default render mode: "fullscreen" | "minimal"
                                       # (unset → fullscreen); set via /settings → Default screen mode

[features]
lsp_tools = false                      # expose the lsp tool
codebase_indexing = true               # code graph indexing
two_pass_compaction = false            # prefire two-pass compaction (default: false, opt-in)

[session]
auto_compact_threshold_percent = 85    # auto-compact at this % of context window
load_envrc = true                      # load .envrc environment variables

[tools]
respect_gitignore = false              # default: false; set true to make every tool skip gitignored files
```

Provider connections are stored in `$ATELIER_HOME/providers.toml`. That file is
connection-only: it may contain endpoints, authentication, discovery, credential
references, and safe extra headers, but no model, Wire API, Role, or
runtime-selection tables. Fixed Role assignments live in `roles.toml` and are
written only after the user configures them. Exact model-ID metadata lives under `models/default/`;
family wildcards such as `gpt-5*` or `claude-*` are rejected. Provider-specific
model and experimental endpoint settings live under
`models/providers/<provider>/`, while discovery results live under
`cache/providers/<provider>/`. Use `/provider`, `/model`, `/wire-api`, and
`/roles` for normal runtime changes.

Each Context always supplies `subagent.md` as the generic subagent system
prompt. Optional `contexts/<preset>/roles/<role>.md` files add Context-specific
Role instructions; a configured subagent Role `prompt_file` is appended after
the Context Role prompt. Goal harness prompts remain under `goal/`.

#### Input Mode

The `simple_mode` setting under `[ui]` controls how you edit text in the
**prompt** — the input editor. It does not change how you navigate the
scrollback; that is governed separately by [`vim_mode`](#vim-mode).

| Value | Behavior |
|-------|----------|
| `true` (default) | **Readline editing.** The prompt uses plain readline-style text entry. |
| `false` | **Vim editing (experimental).** The prompt uses vim-style modal editing (normal and insert modes). When the prompt is empty, it starts in normal mode with focus on the scrollback. |

To switch the prompt to vim-style editing:

```toml
[ui]
simple_mode = false
```

You can also toggle this setting from the settings pane (`/settings` →
**Disable vim input mode**); Atelier writes your choice to `[ui] simple_mode` in
`config.toml`.

`simple_mode` and `vim_mode` are independent: `simple_mode` changes the prompt
editor, and `vim_mode` changes scrollback navigation. See [Keyboard Shortcuts](03-keyboard-shortcuts.md)
for the full binding reference.

#### Default Selected Permission

When the agent asks for permission to run a command (or other tool action),
the approval menu highlights one row by default — the cursor row. The
`default_selected_permission` setting under `[ui]` controls which row that
is on the **first** prompt of a session.

| Value | Preselected row |
|-------|-----------------|
| `always_allow_all_sessions` (default) | The "Always allow on all sessions" row. |
| `allow_command_always` | The "Always allow this command" row. |
| `allow_once` | The "Yes" / allow-once row. |
| `reject` | The reject row. |

```toml
[ui]
default_selected_permission = "allow_once"
```

After you answer the first prompt, the cursor becomes **sticky**: each later
prompt preselects the same kind of choice you last confirmed (e.g. once you
pick "No", subsequent prompts start on their reject row), carrying across edit
/ bash / MCP prompts until you restart. So `default_selected_permission` only
sets the starting point.

The accepted values are `always_allow_all_sessions`, `allow_command_always`,
`allow_once`, and `reject` (matching case-insensitively). When the key is unset
— or set to any unrecognized value — it falls back to `always_allow_all_sessions`.
The `allow_command_always` row is scoped to the specific action being approved
(command / tool / domain / edit-session), never a global allow-everything —
that is `always_allow_all_sessions`. Note that the per-command "Always allow"
rows appear only when `[ui] remember_tool_approvals = true` (default: false).
See [22-permissions-and-safety.md](22-permissions-and-safety.md).

The setting can also be overridden with the `ATELIER_DEFAULT_SELECTED_PERMISSION`
environment variable — handy for headless / agent test runs that shouldn't
mutate `config.toml`. Precedence: env var → `config.toml` →
`always_allow_all_sessions` (the default).

#### Vim Mode

The `vim_mode` setting under `[ui]` controls whether vim-style bindings are
active in the **scrollback** pane. It does not affect the input prompt.

| Value | Behavior |
|-------|----------|
| `false` (default) | Bare-letter and `Shift+letter` keys (`j`/`k`, `h`/`l`, `g`/`G`, `y`/`Y`, `o`/`O`, `r`, `x`, `e`/`E`, `H`/`L`, plus `i`) are suppressed in the scrollback. Pressing one of those letters focuses the prompt and types the character. Arrows, `Tab`, `Space`, `PageUp`/`PageDown`, and all `Ctrl+letter` shortcuts still navigate the scrollback. `Esc` is **not** a scrollback navigation key — it follows clear / rewind / mid-turn-swallow policy (see [Keyboard Shortcuts](03-keyboard-shortcuts.md#escape)). |
| `true` | All vim-style scrollback bindings are active, exactly as listed in [Keyboard Shortcuts](03-keyboard-shortcuts.md). |

Toggle `vim_mode` at runtime with `/vim-mode`, or from the settings pane
(`/settings` → **Vim scrollback navigation**). Atelier writes the change to
`[ui] vim_mode` in `~/.atelier/config.toml` immediately and applies it to every
future pager session — including new agents and subagents started in the same
process. There is no separate per-session override; whatever is in
`config.toml` is the source of truth on next launch.

`vim_mode` is independent of `simple_mode`: `vim_mode` controls scrollback
navigation, while `simple_mode` controls editing in the prompt.

#### Screen Mode

The `screen_mode` setting under `[ui]` is the **default render mode** for plain
`ate` launches. Configure it from `/settings` → **Default screen mode**
(restart required), or edit `config.toml` by hand. Both choices write
`config.toml`. CLI flags (`--minimal` / `--fullscreen`) and slash commands
(`/minimal` / `/fullscreen`) are session-scoped and do **not** write this key —
after a slash switch, the reverse command (`/fullscreen` ⇄ `/minimal`) returns
you for that session only.

| Value | Behavior |
|-------|----------|
| unset | Settings shows **Fullscreen**. At startup there is no sticky preference: legacy `pager.toml` `[terminal] minimal` can still force minimal, and terminals that leak mouse reports (JediTerm/Windows) may auto-open minimal until you set an explicit value. Otherwise the alt-screen policy picks fullscreen vs inline. |
| `"fullscreen"` | Sticky non-minimal. Fullscreen-vs-inline still follows the alt-screen policy (`--no-alt-screen`, `[terminal] alt_screen`, terminal auto-detection). |
| `"minimal"` | Sticky minimal (scrollback-native) mode. |

A CLI flag always wins over the config value for that invocation.

#### Scrolling

Four `[ui]` settings tune mouse-wheel and trackpad scrolling in the
scrollback. All apply immediately (no restart) and are editable from the
settings pane (`/settings` → **Scroll speed** / **Scroll input** /
**Scroll lines** / **Invert scroll**).

| Key | Values (default) | Behavior |
|-----|------------------|----------|
| `scroll_speed` | `1`–`100` (`50`) | Speed multiplier for both wheel and trackpad. `50` = 1.0x, `1` = 0.1x, `100` = 6.0x. |
| `scroll_mode` | `auto` \| `wheel` \| `trackpad` (`auto`) | Wheel-vs-trackpad detection is heuristic (terminal scroll events carry no magnitude); force one kind when auto-detection misreads your device — e.g. a wheel notch that jumps too far, or a trackpad that feels stepped. |
| `scroll_lines` | `1`–`10` (unset) | Lines per scroll tick, applied to **both** wheel and trackpad. While unset, each terminal's own profile applies (e.g. a conservative 1 line/event under tmux). Committing any value — even `3`, the number the settings pane displays — switches permanently to that explicit override. |
| `invert_scroll` | `false` \| `true` (`false`) | Reverse vertical scroll direction ("natural" scrolling). |

```toml
[ui]
scroll_speed = 50
scroll_mode = "auto"     # auto | wheel | trackpad
invert_scroll = false
# scroll_lines is unset by default: the per-terminal profile stays in charge.
# scroll_lines = 3
```

Each setting also has an environment-variable override, applied on first load
only — handy for headless / test runs that shouldn't mutate `config.toml`:
`ATELIER_SCROLL_SPEED`, `ATELIER_SCROLL_MODE`, `ATELIER_INVERT_SCROLL`
(`1`/`true`/`0`/`false`), and `ATELIER_SCROLL_LINES`. Precedence: env var →
`config.toml` → default. Unrecognized values fall back to the default, and
out-of-range numbers clamp to the allowed range.

### Tool Configuration

```toml
[toolset.bash]
timeout_secs = 120.0                   # foreground command timeout in seconds (default: 120)
output_byte_limit = 20000              # max captured output in bytes (default: 20000)

[toolset.ask_user_question]
timeout_enabled = true                 # false = wait forever for answers (default: true)
timeout_secs = 1800                    # seconds to wait when enabled (default: 1800 / 30 min)

[toolset.web_fetch]
proxy_endpoint = "https://proxy.example.com"   # egress proxy URL
allowed_domains = ["docs.rs", "example.com"]    # override the built-in allowlist
```

`[toolset.ask_user_question]` is read from local workspace and user config.
Environment variables
(`ATELIER_ASK_USER_QUESTION_TIMEOUT_ENABLED` /
`ATELIER_ASK_USER_QUESTION_TIMEOUT_SECS`) take precedence over config files.
Set `timeout_enabled = false` in your user config to disable the
automatic questionnaire timeout for yourself; `timeout_secs` must be a
positive integer. `timeout_enabled` can also be toggled from the settings
pane (`/settings` → **Ask-Question timeout**, under Agent & Approval);
changes apply to newly started sessions.

### Providers, Models, and Roles

Provider credentials are explicit references such as `env:NAME`,
`cmd:PROGRAM`, or `none`. Configure them through the TUI:

```text
/provider add allm https://api.example.com/v1 bearer env:ALLM_API_KEY
/provider test allm
/provider refresh allm
/model
/roles
```

See [Provider Credentials](02-authentication.md) and
[Providers, Models, and Roles](11-custom-models.md). Do not place credentials
inside `config.toml`, Role payloads, or model payload overrides.

### MCP Servers

Configure external tool integrations via the Model Context Protocol.

```toml
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }
enabled = true                        # enable/disable (default: true)
startup_timeout_sec = 30              # init timeout in seconds (default: 30)
tool_timeout_sec = 6000               # tool call timeout in seconds (default: 6000)
tool_timeouts = { create_issue = 120 }  # per-tool timeout overrides

[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user:pass@localhost/db"]

[mcp_servers.my-streamable-server]
url = "https://mcp.example.com/api/mcp"  # HTTP/SSE transport
headers = { "x-mcp-session-id" = "{{session_id}}" }
```

MCP servers can also be configured per-project in `.atelier/config.toml`. Project-scoped config contributes `[mcp_servers]`, `[plugins]`, and `[permission]` rules; other sections load only from `~/.atelier/config.toml`.

Priority for `[mcp_servers]` and `[plugins]`: `.atelier/config.toml` (current dir) > `<repo-root>/.atelier/config.toml` > `~/.atelier/config.toml`. `[permission]` rules are not overridden by priority; they merge across all files with `deny` > `ask` > `allow` (see [22-permissions-and-safety.md](22-permissions-and-safety.md)).

### Memory

Persist knowledge across sessions (requires `--experimental-memory` or `ATELIER_MEMORY=1`).

```toml
[memory]
enabled = false                       # enable memory

[memory.session]
save_on_end = true                    # write metadata summary on session end

[memory.watcher]
enabled = true                        # watch memory files for external edits

[memory.search]
max_results = 6                       # default number of results
min_score = 0.35                      # minimum relevance score

[memory.initial_injection]
enabled = true                        # auto-inject memory on first turn
min_score = 0.0                       # score threshold for first-turn injection

[memory.embedding]
model = "embedding-model"             # embedding model name
dimensions = 1024                     # vector dimensions
```

### Subagents

```toml
[subagents]
enabled = true

[subagents.toggle]
explore = true                        # enable/disable specific types
plan = false
```

Built-in subagent model assignment uses the fixed `explore`, `implement`,
`review`, and `test` Roles. Configure them with `/roles`, not with a parent
session model override.

### Skills

```toml
[skills]
paths = ["~/my-team-skills"]          # additional directories to scan
ignore = ["~/my-team-skills/wip"]     # paths to exclude
disabled = ["wip-skill"]              # skill names to keep listed but inactive
```

### Harness Compatibility

Control vendor compatibility for Cursor, Claude, and Codex. Every cell defaults to `true`; session cells remain staged/inert until the foreign-session scanner consumes them.

Session cells remain staged until a foreign-session scanner consumes them. Each tool requires both its `sessions` cell and corresponding `resume-claude`, `resume-codex`, or `resume-cursor` skill; a missing skill means zero foreign-session filesystem I/O.

```toml
[compat.cursor]
skills = true     # scan ~/.cursor/skills/ and <cwd>/.cursor/skills/
rules = true      # scan <cwd>/.cursor/rules/
agents = true     # scan ~/.cursor/ for AGENTS.md files
mcps = true       # scan ~/.cursor/mcp.json and <cwd>/.cursor/mcp.json
hooks = true      # scan ~/.cursor/hooks.json and <cwd>/.cursor/hooks.json
sessions = true   # staged; no scanner consumer yet

[compat.claude]
skills = true     # scan ~/.claude/skills/ and <cwd>/.claude/skills/
rules = true      # scan <cwd>/.claude/rules/
agents = true     # scan ~/.claude/ for CLAUDE.md / CLAUDE.local.md
mcps = true       # scan ~/.claude.json for MCP servers
hooks = true      # scan ~/.claude/settings.json for hooks
sessions = true   # staged; no scanner consumer yet

[compat.codex]
sessions = true   # staged; no scanner consumer yet
```

Codex `skills`, `rules`, `agents`, `mcps`, and `hooks` cells are reserved and currently inert; they do not enable `.codex` discovery.

Each cell can be toggled via environment variable or `config.toml`. See the
environment-variables reference for the env var names. Resolution order:
env var > config.toml > default (on).

`ate inspect` reports cells that still need session-start resolution as
`?` until a value is available; cells with an explicit env or TOML value
use that value. Affected discovery entries report
`compatibilityStatus: "unresolved"` in JSON and `[compat unresolved]` in
human output.

### Plugins

```toml
[plugins]
paths = ["~/my-plugins/custom-tools"]
disabled = ["user/a1b2c3d4/noisy-plugin"]
```

### Hints

The `[hints]` table holds small persisted UI preferences — mostly "stop asking me" opt-outs. Atelier writes these for you when you pick a "don't ask again" / "reset in config.toml" option in the TUI, but you can edit or remove them by hand. Deleting a key restores the default behavior.

`[hints]` is read from the normal local config merge. The TUI writes opt-outs
only to `$ATELIER_HOME/config.toml`.

```toml
[hints]
project_picker_disabled = false        # skip the project-directory picker
memory_modal_fullscreen = false        # remember the memory modal fullscreen state
new_session_worktree_mode = "never"    # /new worktree prompt: "ask" | "always" | "never"
fork_worktree_mode = "ask"             # /fork worktree prompt: "ask" | "always" | "never"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `project_picker_disabled` | bool | `false` | When `true`, skips the picker that asks you to choose a project directory on the first prompt when Atelier is launched from a non-project directory (home, Desktop, Downloads, `/tmp`). Set automatically when you choose **"Don't ask me again"** in that picker. |
| `memory_modal_fullscreen` | bool | `false` | Remembers whether the memory modal was last opened fullscreen. |
| `new_session_worktree_mode` | string | `"never"` | Worktree prompt for `/new`: `ask` shows the popup, `always` creates a worktree, `never` skips it. |
| `fork_worktree_mode` | string | `"ask"` | Worktree prompt for `/fork`: `ask`, `always`, or `never`. |

### Notifications

Send terminal notifications when the agent finishes a turn or needs
approval. Notifications use terminal-native protocols (OSC 9, OSC 99, OSC 777,
or BEL) and are focus-gated by default so they only fire when you are not
looking at the terminal.

```toml
[ui.notifications]
method = "auto"           # auto|osc9|osc99|osc777|bel|none
condition = "unfocused"   # unfocused|always|never
idle_threshold_secs = 3   # seconds unfocused before a notification fires
events = ["turn_complete", "approval_required"]
sleep_prevention = true   # prevent display sleep during agent turns
progress_bar = true       # show tab progress bar (OSC 9;4)

[ui.notifications.title]
enabled = true
items = ["action-required", "spinner", "activity", "session-name", "ate"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `method` | string | `"auto"` | Notification protocol. `auto` picks the best for your terminal. |
| `condition` | string | `"unfocused"` | When to notify: `unfocused` (only when terminal lost focus), `always`, or `never`. |
| `idle_threshold_secs` | integer | `3` | Minimum seconds the terminal must be unfocused before a notification fires. |
| `events` | array | `["turn_complete", "approval_required"]` | Events that trigger notifications. Options: `turn_complete`, `approval_required`, `session_ready`, `task_complete`, `agent_error`. |
| `sleep_prevention` | bool | `true` | Keep the display awake while the agent is working (macOS/Linux). |
| `progress_bar` | bool | `true` | Show a progress indicator in the terminal tab (OSC 9;4). |
| `title.enabled` | bool | `true` | Set the terminal title to reflect agent state. |
| `title.items` | array | (see above) | Items shown in the title bar. Options: `action-required`, `spinner`, `activity`, `session-name`, `cwd`, `model`, `turn-timer`, `ate`. |

#### Terminal Support Matrix

| Terminal | Auto Protocol | Focus Tracking | Progress Bar |
|----------|---------------|----------------|--------------|
| iTerm2 | OSC 9 | Yes | Yes |
| Kitty | OSC 99 | Yes | No |
| Ghostty | OSC 777 | Yes | Yes |
| WezTerm | OSC 9 | Yes | Yes |
| Warp | OSC 9 | Yes | No |
| Alacritty | BEL | Yes | No |
| VS Code | BEL | Yes | No |
| Apple Terminal | BEL | No | No |
| VTE (GNOME Terminal) | OSC 777 | Yes | No |
| Atelier Desktop | None (native) | N/A | N/A |
| Unknown | BEL | No | No |

When `method = "auto"`, Atelier detects the terminal brand and selects the best
protocol automatically. Set `method` explicitly to override auto-detection.

#### Notification Hooks

Run custom commands when events occur. Hooks receive environment variables
`$ATELIER_EVENT`, `$ATELIER_MESSAGE`, and `$ATELIER_SESSION_ID`.

```toml
# macOS native notification
[[ui.notifications.hooks]]
command = "terminal-notifier -title 'Atelier' -message '$ATELIER_MESSAGE'"
events = ["turn_complete", "approval_required"]
only_unfocused = true
timeout_secs = 10

# Push to ntfy server
[[ui.notifications.hooks]]
command = "curl -s -d '$ATELIER_MESSAGE' ntfy.sh/my-atelier-alerts"
events = ["turn_complete"]
only_unfocused = true
timeout_secs = 10

# Play a sound
[[ui.notifications.hooks]]
command = "afplay /System/Library/Sounds/Glass.aiff"
events = ["turn_complete"]
only_unfocused = true
timeout_secs = 5
```

| Hook Option | Type | Default | Description |
|-------------|------|---------|-------------|
| `command` | string | (required) | Shell command to run. |
| `events` | array | `[]` | Events that trigger this hook (empty = all events). |
| `only_unfocused` | bool | `true` | Only fire when the terminal has lost focus. |
| `timeout_secs` | integer | `10` | Kill the hook process after this many seconds (default: 10). |

#### Troubleshooting

**Notifications not working in tmux:**
tmux blocks escape sequences by default. Enable passthrough for your terminal:

```bash
# In ~/.tmux.conf
set -g allow-passthrough on
```

Then restart tmux. If passthrough is not available (tmux < 3.3), set
`method` explicitly to `"bel"` which works without passthrough.

**Focus tracking not working:**
Some terminals do not report focus events. If `condition = "unfocused"` never
fires, try `condition = "always"` as a fallback. Atelier supports focus tracking
in every detected terminal except Apple Terminal and unrecognized terminals.

**Sleep prevention not taking effect:**
On macOS, sleep prevention uses `IOPMAssertionCreateWithName` via CoreFoundation.
On Linux, it uses `systemd-inhibit` (must be on `$PATH`). Check that the
relevant tool is available. Sleep prevention is only active during agent turns
and releases automatically when the turn ends.

### Keyboard Shortcuts

Keyboard shortcuts are **not configurable** via config files. All bindings are built in.
See [Keyboard Shortcuts](03-keyboard-shortcuts.md) for the complete reference.

### Telemetry

Atelier has no remote telemetry, product analytics, crash-reporting, or trace
uploader. Runtime diagnostics are local only.

```bash
ate --debug
ate --debug-file C:\tmp\atelier-debug.log
```

See [Local Diagnostics](24-monitoring-usage.md).

### Local Deployment Example

A minimal local Runtime config keeps Provider/model assignment outside
`config.toml`:

```toml
[features]
lsp_tools = false
codebase_indexing = true

[session]
auto_compact_threshold_percent = 85
```

Configure the actual endpoint, credential, discovered models, and Roles with
`/provider` and `/roles`. For isolated CI state, set `ATELIER_HOME` to a
dedicated writable directory.

---

## pager.toml (Appearance Configuration)

Location: `~/.atelier/pager.toml`

Controls the visual appearance and behavior of the TUI. Changes are applied on restart.

### Terminal

```toml
[terminal]
alt_screen = "auto"                   # fullscreen mode: "auto", "always", "never"
```

- `auto` (default): Use alternate screen when the terminal supports it
- `always`: Always use alternate screen
- `never`: Run inline in the terminal's main scrollback buffer

### Animation

```toml
[animation]
fps = 30                              # animation frame rate (ticks per second)
wave_rows = 32                        # rows per wave cycle for accent animation
```

### Prompt

```toml
[prompt]
collapse_unfocused = true             # collapse prompt when scrollback is focused
mouse_hover = true                    # show hover highlight on the prompt widget
show_prefix = true                    # show the prompt prefix character
```

Compact mode is not persisted here. Control it at runtime with `[ui] compact_mode` or the `/compact-mode` command.

### Scrollback

```toml
[scrollback.layout]
outer_vpad = 1                        # vertical padding
outer_hpad_left = 2                   # left horizontal padding
outer_hpad_right = 2                  # right horizontal padding
block_pad_left = 2                    # padding inside block, left of content
block_pad_right = 2                   # padding inside block, right of content

[scrollback.scrollbar]
enabled = true                        # show scrollbar
gap_left = 0                          # gap between content and scrollbar
gap_right = 0                         # gap between scrollbar and screen edge

[scrollback.scroll]
margin = 0                            # minimum context lines above/below selection
min_page_fraction = 0                 # minimum scroll as % of viewport (0-100)
follow_indicator = "center"           # follow indicator: "center" or "none"
follow_auto_select = true             # auto-select latest entry in follow mode
follow_by_overscroll = true           # scrolling past bottom engages follow mode
anchor_on_fold = true                 # keep block position when folding
respect_manual_folds = true           # opt-in (default: false): keep manually folded blocks as-is during streaming/finish; expanding while following stops auto-scroll

[scrollback.display]
sticky_headers = true                 # pin user prompts as sticky headers
tab_width = 4                         # spaces per tab character
expandable_indicator = true           # show expand indicator on foldable entries
expandable_indicator_running = true   # show indicator on running entries
expandable_indicator_char = "›"       # character for the expand indicator (default: "›")
selection_buttons = false             # show copy/view buttons on selection
line_under_last_entry = false         # horizontal line below last entry
group_selection_split = true          # split selection box for expanded blocks
highlight_overlays_border = false     # highlight extends over selection box border
dim_accent = 0.5                      # dimming factor for collapsed accents (0.0-1.0)
```

`respect_manual_folds` is off by default; set it to `true` to opt in. When
enabled, a block you fold by hand is pinned: streaming updates and finish
events (such as a thinking block ending) leave its fold state alone, and
expanding a block while follow-mode is tailing new content stops the
auto-scroll so the view stays put. Follow resumes via `Shift+G`, `j` at the
last entry, scrolling past the bottom, or sending a new prompt. `Shift+E`
clears all pins; `Ctrl+E` clears pins on thinking blocks.

### Block Configuration

```toml
[scrollback.blocks.edit]
indent = true                         # indent diff content
vpad = false                          # vertical padding
# expanded_by_default = true          # unset: follows [ui] collapsed_edit_blocks in config.toml
                                      # (flag on = collapsed one-liner); uncomment to pin either shape
dual_line_numbers = false             # two-column line numbers (old + new)
# line_summary = false                # show +N/-M in the collapsed header; unset follows the same flag
hunk_separator = "…"                  # separator between diff hunks (default: "…")

[scrollback.blocks.prompt]
vpad = true                           # vertical padding
show_prefix = true                    # show prompt prefix character
min_lines = 2                         # minimum content lines in sticky mode

[scrollback.blocks.thinking]
animate = true                        # animated accent while thinking
truncated_lines = 3                   # lines in truncated mode
```

### Todo

```toml
[todo]
badge_format = "default"              # "default", "colon", or "comma"
```

Badge format examples:
- `default`: `2/5` -- a `done/total` progress fraction (done = completed, total = all tasks except cancelled)
- `colon`: `[>:1 [ ]:4 ok:3 x:2]` -- icon:count
- `comma`: `[1 >, 4 [ ], 3 ok, 2 x]` -- count icon, comma-separated

### Plugins

```toml
disable_plugins = false               # hide hooks/plugins UI entirely
```

---

## Environment Variables

Key environment variables. See the README for the complete list.

### Provider Credentials

Provider credentials use the environment variable named in the Provider's
`env:NAME` reference. Atelier does not define a global model API-key variable.

```text
/provider add allm https://api.example.com/v1 bearer env:ALLM_API_KEY
```

In that example, only `ALLM_API_KEY` is read for Provider `allm`.

### Features

| Variable | Description |
|----------|-------------|
| `ATELIER_MEMORY` | Enable (`1`) or disable (`0`) cross-session memory |
| `ATELIER_SUBAGENTS` | Enable (`1`) or disable (`0`) subagents |
| `ATELIER_WEB_FETCH` | Enable (`1`) or disable (`0`) the web_fetch tool |
| `ATELIER_AGENT` | Custom agent definition path or name |
| `ATELIER_SANDBOX` | Sandbox profile (off, workspace, devbox, read-only, strict; or a custom profile name) |

### Logging

| Variable | Description |
|----------|-------------|
| `ATELIER_LOG_FILE` | Write logs to this file path (the value is used verbatim as the path) |
| `RUST_LOG` | Log level filter (for example `debug`); controls the `ATELIER_LOG_FILE` log and headless stderr output |

### Paths

| Variable | Description |
|----------|-------------|
| `ATELIER_HOME` | Override config directory (default: `~/.atelier`) |
| `ATELIER_RESPECT_GITIGNORE` | Force gitignore filtering on (`1`) or off (`0`); overrides `[tools] respect_gitignore` |

### Local Diagnostics

| Variable | Description |
|----------|-------------|
| `ATELIER_DEBUG_LOG` | `1` writes per-session logs; a path writes one local file |
| `ATELIER_LOG_FILE` | Write local logs to this exact file path |
| `RUST_LOG` | Local log filter, for example `debug` |

---

## File Locations

| Path | Description |
|------|-------------|
| `$ATELIER_HOME/config.toml` | Main configuration file |
| `$ATELIER_HOME/providers.toml` | Provider API/OAuth connection registry |
| `$ATELIER_HOME/roles.toml` | Fixed Role to Provider/model assignments |
| `$ATELIER_HOME/request-agents.toml` | Selectable outbound identities with explicit User-Agent strings |
| `$ATELIER_HOME/models/default/` | Exact model-ID defaults: effort, fast mode, context window, capabilities, and Wire API |
| `$ATELIER_HOME/models/providers/<provider>/` | Provider-specific model overrides and experimental endpoints |
| `$ATELIER_HOME/cache/providers/<provider>/` | Provider discovery cache; never stored in `providers.toml` |
| `$ATELIER_HOME/contexts/<preset>/` | Editable generic, Goal, compaction, and Role prompt files |
| `$ATELIER_HOME/branding/logo.txt` | TUI ASCII logo |
| `$ATELIER_HOME/credentials/oauth/providers/` | Provider OAuth credentials |
| `$ATELIER_HOME/credentials/oauth/mcp/` | MCP OAuth credentials |
| `$ATELIER_HOME/pager.toml` | TUI appearance configuration |
| `$ATELIER_HOME/sessions/` | Persisted sessions (organized by working directory) |
| `$ATELIER_HOME/memory/` | Cross-session memory files and index |
| `$ATELIER_HOME/skills/` | User-scoped skill definitions |
| `$ATELIER_HOME/plugins/` | User-scoped plugins |
| `$ATELIER_HOME/agents/` | User-scoped agent definitions |
| `$ATELIER_HOME/lsp.json` | LSP server configuration (user-scoped) |
| `$ATELIER_HOME/logs/` | Local log files (for example `unified.jsonl`, MCP server logs) |
| `~/.atelier/bin/ate` | Installed executable (`ate.exe` on Windows); independent of a custom Runtime `ATELIER_HOME` |
| `.atelier/config.toml` | Project-scoped MCP servers, plugins, and permission rules |
| `.atelier/skills/` | Project-scoped skill definitions |
| `.atelier/plugins/` | Project-scoped plugins |
| `.atelier/agents/` | Project-scoped agent definitions |
| `.atelier/hooks/` | Project-scoped hooks |
| `.atelier/lsp.json` | LSP server configuration |

---

## Project-Scoped Configuration

Some configuration can be set per-project by placing files in `.atelier/` within your repository:

| File | What it configures |
|------|--------------------|
| `.atelier/config.toml` | MCP servers, plugins, permission rules, and the `[mcp] max_output_bytes` tool-result cap (other sections load only from `~/.atelier/config.toml`) |
| `.atelier/skills/` | Project-specific skills |
| `.atelier/hooks/` | Project-specific lifecycle hooks |
| `.atelier/agents/` | Project-specific agent definitions |
| `.atelier/lsp.json` | LSP server configuration |
| `.atelier/sandbox.toml` | Custom sandbox profiles |
| `AGENTS.md` | Project instructions (system prompt) |

Project-scoped MCP servers override global ones with the same name (full replacement, not merge).

---

## LSP Servers

Language servers power passive diagnostics and the optional `lsp` tool (see the [`lsp_tools`](#general-settings) feature flag). Server definitions are collected from three sources and merged by server name:

| Source | Location | Scope |
|--------|----------|-------|
| User | `~/.atelier/lsp.json` | All projects |
| Project | `.atelier/lsp.json` | Current repository |
| Plugin | A trusted plugin's `.lsp.json` file, or an inline `lspServers` block in its `plugin.json` | Wherever the plugin is enabled |

When the same server name is defined by more than one source, it is resolved in this order (highest priority first):

1. **Project** -- `.atelier/lsp.json`
2. **User** -- `~/.atelier/lsp.json`
3. **Plugins** -- file-based `.lsp.json`, then inline `lspServers`, in plugin load order

Project and user entries replace lower-priority ones with the same name. Plugin entries only add servers whose names are not already defined by a local file, so a local `lsp.json` always wins over a plugin. Plugin LSP servers load only after the plugin is trusted (see [Plugins](09-plugins.md)).
