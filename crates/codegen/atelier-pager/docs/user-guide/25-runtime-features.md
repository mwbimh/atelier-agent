# Runtime features

Atelier exposes advanced session orchestration through TUI slash commands and
versioned ACP extensions. A future GUI can use the same interfaces.

## Derived sessions

Create focused work from the current session:

```text
/agent explore find the Provider reload path
/agent explore --append "check connection cleanup" find the reload path
/agent --fresh explore analyze the project from scratch

/fork
/fork --append "focus on the Windows sandbox"

/parallel explore inspect Provider config; review inspect Wire API; test design regressions
```

Derived agents use an immutable `ContextSnapshot` created at spawn time. The
snapshot includes completed visible conversation content and excludes system
prompts, credentials, tool schemas, permission state, sandbox handles, raw
reasoning, and unfinished streamed messages. Text supplied with `--append` is
placed after the snapshot and the task itself is always sent last.

## BTW questions

Use `/btw` for a side question that should not modify the main session:

```text
/btw why did the previous request use the Responses API?
```

BTW performs one model request without tools. It does not compact the main
conversation or change the active Role or Plan mode.

The result panel supports:

```text
C    copy the answer
P    save the answer to the local btw_history.jsonl file
Esc  close the panel
```

Saving records the existing answer and does not call the model again. Answers
are not persisted unless explicitly saved.

## Background tasks

```text
/background
/bg
/tasks
/attach <task-id>
/fg <task-id>
/stop <task-id>
```

`/background` detaches the current turn from the foreground while model and tool
work continues. `/tasks` lists task state. `/attach` replays events after the
client's last Event ID, switches to the owning session when needed, and
subscribes to future updates for a running task. `/stop` cancels the task and
its process tree.

Runtime tasks, request snapshots, and replay buffers are owned by the running
Atelier runtime. Without a persistent leader process, tasks do not survive
process exit.

## Model Wire API

Inspect or change model protocol selection at runtime:

```text
/wire-api list
/wire-api get provider/model
/wire-api wire provider/model responses
/wire-api override provider/model chat_completions {"temperature":0.2}
/wire-api test provider/model
/wire-api test provider/model execute
/wire-api delete provider/model
```

Resolution order is:

```text
Provider-model override
→ model wire_api
→ chat_completions
```

Changes apply to the next request. In-flight requests keep their existing
snapshot. Atelier does not silently probe another protocol or fall back to a
different model.

Supported values:

```text
chat_completions
responses
messages
default
```

Request and context inspection records the resolved Wire API and its source;
sensitive payload fields are redacted.

## ACP extensions

Advanced runtime methods include:

```text
_atelier/context_snapshot/create
_atelier/context_snapshot/get
_atelier/context_snapshot/list
_atelier/context_snapshot/delete
_atelier/agent/spawn_derived
_atelier/agent/spawn_parallel
_atelier/btw/ask
_atelier/btw/get
_atelier/btw/list
_atelier/btw/delete
_atelier/btw/persist
_atelier/task/list
_atelier/task/get
_atelier/task/detach
_atelier/task/attach
_atelier/task/cancel
_atelier/task/subscribe
_atelier/model/get
_atelier/model/update_wire_api
_atelier/model_provider_override/list
_atelier/model_provider_override/set
_atelier/model_provider_override/delete
_atelier/model_provider_override/test
```
