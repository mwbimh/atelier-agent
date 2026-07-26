# Agent Mode (ACP) and IDE Integration

Agent mode runs Atelier as an ACP (Agent Client Protocol) server for integration with IDEs, editors, and custom tooling. Unlike single-prompt mode (`ate -p`, which prints one response and exits), agent mode keeps a persistent process running and communicates through structured JSON-RPC messages.

---

## What is ACP?

The [Agent Client Protocol (ACP)](https://agentclientprotocol.com) is a standard for AI agent communication. It defines how clients (IDEs, editors, custom apps) interact with AI agents through a structured JSON-RPC protocol. ACP provides:

- **Session management** -- create, load, and resume conversations
- **Prompt submission** -- send user messages and receive streamed responses
- **Tool visibility** -- see what tools the agent is using in real time
- **Thought streams** -- observe the agent's reasoning process
- **Permission handling** -- approve or deny tool executions interactively

---

## stdio transport

stdio is the primary integration mode. The agent exchanges JSON-RPC messages over stdin and stdout:

```bash
ate agent stdio
```

Clients that use this mode include:

- IDE extensions (for example, Zed, Neovim, and Emacs)
- Custom automation tools
- ACP client libraries

### Options

These options belong to the `ate agent` command and apply to every mode. Pass them before the mode name, for example `ate agent --model example/deepseek-v4-flash stdio`. The `stdio` subcommand itself takes no options.

| Flag                       | Description                                                       |
| -------------------------- | ---------------------------------------------------------------- |
| `-m, --model <MODEL>`      | Set a configured model key (for example, `example/deepseek-v4-flash`). |
| `--always-approve`         | Auto-approve every tool execution. (Alias: `--yolo`.)            |
| `--agent-profile <PATH>`   | Load an agent profile from a file.                               |

---

## Server mode

Run the agent as a WebSocket server for remote clients:

```bash
ate agent serve --bind 127.0.0.1:2419 --secret <token>
```

Clients connect over WebSocket and authenticate with the secret token. If you omit `--secret`, the agent generates a token and prints it at startup; you can also supply one through the `ATELIER_AGENT_SECRET` environment variable. The agent persists across reconnections, so a client can disconnect and later resume in-flight work.

The server binds to loopback by default. Exposing it beyond the local machine is
an explicit deployment decision and requires an external authentication,
transport-security, and network-boundary review.

---

## ACP protocol basics

Communication follows the JSON-RPC 2.0 format. A typical session lifecycle:

1. **Initialize** -- client sends `initialize` with capabilities
2. **Create session** -- client sends `session/new` with working directory
3. **Send prompts** -- client sends `session/prompt` with user messages
4. **Receive updates** -- agent sends `session/update` notifications with streamed content
5. **Handle permissions** -- agent may request tool execution approval

### Architecture

```
+------------------------------------------+
|           ACP Client                     |
|  (IDE, Editor, Custom Application)       |
+-------------------+----------------------+
                    | JSON-RPC over stdio
+-------------------v----------------------+
|             ate agent stdio                 |
|                                          |
|  +---------+  +---------+  +---------+   |
|  | Session |  |  Tools  |  |   MCP   |   |
|  | Manager |  | Registry|  | Servers |   |
|  +---------+  +---------+  +---------+   |
+------------------------------------------+
```

---

## Streaming updates

ACP streams structured events. Each `session/update` notification carries a `sessionUpdate` field that identifies the update type:

| `sessionUpdate` value | Description                                            |
| --------------------- | ----------------------------------------------------- |
| `agent_message_chunk` | A chunk of the agent's response text.                 |
| `agent_thought_chunk` | A chunk of the agent's internal reasoning.            |
| `tool_call`           | A new tool invocation (title, kind, status, input).   |
| `tool_call_update`    | A status or result update for an in-flight tool call. |
| `plan`                | The agent's execution plan.                           |

Each update names its type, so a client can render distinct panels for reasoning, tool calls, and response text.

---

## Extension methods

Beyond base ACP, Atelier defines versioned Runtime extensions under the
`_atelier/` namespace. Representative groups include:

| Category | Prefix | Examples |
|---|---|---|
| Provider registry | `_atelier/provider/*` | `list`, `create`, `update`, `enable`, `test`, `delete` |
| Model configuration | `_atelier/model/*` | `list`, `get`, `update_wire_api` |
| Provider/model overrides | `_atelier/model_provider_override/*` | `set`, `delete`, `test` |
| Fixed Roles | `_atelier/role/*` | `list`, `get`, `update`, `update_payload`, `test` |
| Context and request inspection | `_atelier/context/*`, `_atelier/request/*` | `current`, `list`, `get` |
| Runtime diagnostics | `_atelier/runtime/*` | `status`, `doctor`, `cancel`, `retry`, `recover`, `tasks` |
| Runtime task control | `_atelier/task/*` | create, list, attach, stop, and replay operations |

Use protocol version/capability negotiation instead of assuming every
extension exists. Provider credentials remain local references; ACP clients do
not need a product account or login flow.

### Notifications (agent to client)

The agent sends push notifications to clients for real-time updates:

| Notification               | Description                          |
| -------------------------- | ------------------------------------ |
| `session/update` | Standard ACP session updates (tool calls and content) |
| `atelier/task/update` | Background task status, replay, and result updates |
| Runtime extension notifications | Capability-specific Provider, model, Role, and diagnostic changes |

---

## Session `_meta` options

The `session/new` request accepts these optional `_meta` fields:

| Field                  | Description                                    |
| ---------------------- | ---------------------------------------------- |
| `rules`                | Extra rules appended to the system prompt.     |
| `systemPromptOverride` | A replacement system prompt.                   |
| `agentProfile`         | An agent profile, as a name or a JSON object.  |

---

## ACP SDKs

Official SDK libraries are available for multiple languages:

| Language   | Package                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------- |
| TypeScript | [`@agentclientprotocol/sdk`](https://www.npmjs.com/package/@agentclientprotocol/sdk)     |
| Rust       | [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol)                |
| Python     | [`agent-client-protocol-python`](https://github.com/PsiACE/agent-client-protocol-python) |
| Go         | [`acp-go-sdk`](https://github.com/coder/acp-go-sdk)                                     |
| Kotlin     | [`acp`](https://github.com/agentclientprotocol/kotlin-sdk)                               |

---

## Compatible clients

| Client                                                   | Status      |
| -------------------------------------------------------- | ----------- |
| [Zed](https://zed.dev/docs/ai/external-agents)           | Supported   |
| [Neovim](https://neovim.io) (CodeCompanion, avante.nvim) | Supported   |
| [Emacs](https://github.com/xenodium/agent-shell)         | Supported   |
| [marimo notebook](https://github.com/marimo-team/marimo) | Supported   |
| JetBrains                                                | Coming soon |

---

## Integration example: a TypeScript ACP client

```typescript
import { spawn, ChildProcess } from "child_process";
import * as readline from "readline";

class AtelierACPChat {
  private proc!: ChildProcess;
  private sessionId!: string;
  private rl!: readline.Interface;

  constructor(private cwd = ".") {}

  async init() {
    this.proc = spawn("ate", ["agent", "stdio"]);
    this.rl = readline.createInterface({ input: this.proc.stdout! });

    // Initialize
    await this.request("initialize", {
      protocolVersion: 1,
      clientCapabilities: {
        fs: { readTextFile: true, writeTextFile: true },
        terminal: true,
      },
    });

    // Create session
    const { sessionId } = await this.request("session/new", {
      cwd: this.cwd,
      mcpServers: [],
    });
    this.sessionId = sessionId;
    return this;
  }

  private async request(method: string, params: any): Promise<any> {
    return new Promise((resolve) => {
      const msg = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
      this.proc.stdin!.write(msg + "\n");

      this.rl.once("line", (line) => {
        resolve(JSON.parse(line).result || {});
      });
    });
  }

  async *streamPrompt(text: string) {
    const msg = JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "session/prompt",
      params: {
        sessionId: this.sessionId,
        prompt: [{ type: "text", text }],
      },
    });
    this.proc.stdin!.write(msg + "\n");

    for await (const line of this.rl) {
      const data = JSON.parse(line);

      if (data.method === "session/update") {
        const update = data.params.update;
        yield update; // { sessionUpdate, content, title, ... }
      } else if (data.result) {
        break; // Final response
      }
    }
  }
}

// Usage
const client = await new AtelierACPChat(".").init();

for await (const update of client.streamPrompt("List the files in this project")) {
  switch (update.sessionUpdate) {
    case "agent_message_chunk":
      process.stdout.write(update.content?.text || "");
      break;
    case "agent_thought_chunk":
      console.log(`\n[Thinking: ${update.content?.text}]`);
      break;
    case "tool_call":
      console.log(`\n[Tool: ${update.title}]`);
      break;
  }
}
```

---

## Resources

- [ACP Specification](https://agentclientprotocol.com/protocol/prompt-turn)
- [Protocol Introduction](https://agentclientprotocol.com/overview/introduction)
