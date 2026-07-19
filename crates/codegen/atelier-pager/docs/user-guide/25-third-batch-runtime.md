# Third-batch Runtime features

第三批 Runtime 功能先通过 TUI slash command 和版本化 ACP extension 提供，桌面 GUI 可以复用同一套接口。

## 从当前会话派生

完整命令和入口交互都支持：

```text
/agent explore 查找 Provider 热更新路径
/agent explore --append "重点检查旧连接释放" 查找 Provider 热更新路径
/agent --fresh explore 从零分析项目结构

/fork
/fork --append "只讨论 Windows 沙箱"

/parallel explore 查 Provider 配置; review 审查 Wire API; test 设计回归测试
```

直接输入 `/agent` 或 `/parallel` 会进入角色选择/命令参数输入。`/fork` 会进入现有 worktree 选择流程。

派生 Agent 使用创建时冻结的 `ContextSnapshot`。快照只包含已完成、可见的对话内容，不包含 system prompt、凭据、工具 schema、权限状态、sandbox 句柄、reasoning 原文或未完成的流式消息。`--append` 位于快照之后，实际任务始终最后发送。

现有 main → task subagent、resume、compact、summary 和 title 路径不经过这套显式快照机制。

## BTW 旁路提问

```text
/btw 为什么刚才使用 Responses？
```

BTW 使用一次无工具模型请求，不修改主会话、不触发 compact，也不改变 Role 或 Plan Mode。面板支持：

```text
C 复制答案
P 保存当前原答案到本地 btw_history.jsonl
Esc 关闭面板
```

`P` 保存的是当前面板对应的 `btwId` 和原始答案，不会重新调用模型。默认不持久化；保存后可通过 `_atelier/btw/get`、`_atelier/btw/list` 查询。

## 后台运行、查看和恢复

```text
/background
/bg
/tasks
/attach <task-id>
/fg <task-id>
/stop <task-id>
```

`/background` 让当前 turn 脱离前台并打开 dashboard；模型和工具继续运行。`/tasks` 显示任务状态，`WaitingForPermission` 会显示为 `NEEDS INPUT`。`/attach` 会按客户端最后的 Event ID 返回 replay，在任务属于其他 session 时切换到该 session，并为仍在运行的任务建立实时订阅；后续更新通过 `atelier/task/update` 通知发送。标记为 `RESULT ONLY` 的 BTW、Compact、Summary 和 Title 任务不可 attach，只能读取结果或状态。`/stop` 取消对应 session 的当前任务和进程树。

Runtime task、request snapshot 和 replay buffer 当前由运行中的 Atelier Runtime 持有。没有 daemon/Leader 时，进程退出后任务不会继续执行；第三批不承诺跨进程恢复。

## Model-level Wire API

模型默认协议和 Provider + Model 覆盖可以在运行时修改：

```text
/model-config list
/model-config get proxy/gpt-5
/model-config wire proxy/gpt-5 responses
/model-config override proxy/gpt-5 chat_completions {"temperature":0.2}
/model-config test proxy/gpt-5
/model-config test proxy/gpt-5 execute
/model-config delete proxy/gpt-5
```

直接输入 `/model-config` 或 `/models` 会进入参数交互入口。解析顺序是：

```text
Provider-Model override
→ Model wire_api
→ chat_completions
```

Provider 不决定所有模型的 Wire API。配置修改从下一次请求生效，正在执行的请求继续使用旧快照；不会自动 fallback 或自动探测协议。

支持的值：

```text
chat_completions
responses
messages
default
```

所有 request/context inspector 快照都会记录最终 Wire API 和解析来源，敏感 payload 会脱敏。

## ACP extension

第三批扩展方法包括：

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
