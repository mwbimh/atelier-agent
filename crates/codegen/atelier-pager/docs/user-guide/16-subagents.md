# Subagents

Subagents are independent child sessions used for delegated exploration, implementation, review, testing, planning, and verification. Each child has its own context window and reports its result to the parent.

Subagents are enabled by default.

## Fixed Runtime Roles

Atelier has a fixed Role set. Users cannot add custom Runtime Roles, personas, or agent presets.

| Role | Purpose |
|---|---|
| `main` | Main interactive session. Displayed as `MAIN` in the Role UI. |
| `general` | Default general-purpose Subagent. |
| `explore` | Read-oriented codebase exploration. |
| `implement` | Code implementation. |
| `review` | Correctness, regression, and security review. |
| `test` | Test execution and diagnosis. |
| `compact` | Conversation compaction. |
| `summary` | Session recap and summary generation. |
| `title` | Session title generation. |
| `planner` | Goal planning. |
| `strategist` | Goal strategy. |
| `skeptic` | Goal verification and challenge. |

The fixed execution inheritance tree is:

```text
MAIN
├─ General
│  ├─ Explore
│  ├─ Implement
│  ├─ Review
│  └─ Test
├─ Compact
├─ Summary
├─ Title
├─ Planner
├─ Strategist
└─ Skeptic
```

`MAIN` reads its model from `config.toml`. Other Role overrides are stored in `roles.toml`. Execution settings resolve independently by field: `provider`, `model`, `effort`, `fast_mode`, and payload keys can each come from a different point in the fixed parent chain. Parent payload keys are merged before child keys. Missing fields inherit; an explicitly invalid field fails closed.

The final exact `provider/model` determines the Wire API. For example, an unconfigured Compact Role inherits the active Main model and therefore uses that model's Responses, Chat Completions, or Messages adapter.

## General Subagent

`general-purpose` and unrecognized generic delegation paths resolve to the fixed `general` Role. Specialized built-in types resolve as follows:

| Subagent type | Runtime Role |
|---|---|
| `general-purpose` | `general` |
| `explore` | `explore` |
| `implement` | `implement` |
| `review` | `review` |
| `test` | `test` |
| `plan` | `planner` |

Custom `[subagents.roles.*]` and `[subagents.personas.*]` tables are rejected as unknown configuration. `.atelier/roles/*.toml`, `.atelier/personas/*.toml`, `.atelier/agents/*.md`, compatibility Agent directories, and plugin Agent presets are not discovered. `[agent].definition`, custom `[agent].name` values, `--agent-profile`, headless `--agents`, and file-valued `--agent` inputs are rejected. The legacy `/config-agents` and `/personas` management commands are not registered.

## Context Packages

A Session selects one Context package under:

```text
~/.atelier/contexts/<package>/
```

A package can provide fixed Role Context files:

```text
roles/main.md
roles/general.md
roles/explore.md
roles/implement.md
roles/review.md
roles/test.md
roles/compact.md
roles/summary.md
roles/title.md
roles/planner.md
roles/strategist.md
roles/skeptic.md
```

Role Context resolution searches the selected package first, then `default`.

Non-Main Context inheritance never reaches Main:

```text
review → general → stop
compact → stop
main → main
```

For Review, the order is:

```text
selected/roles/review.md
selected/roles/general.md
default/roles/review.md
default/roles/general.md
empty
```

An existing empty file is authoritative: it contributes no text and stops fallback. A missing file continues fallback. A read error fails closed.

`subagent.md` remains the common Subagent protocol layer. A resolved fixed Role Context is appended after that protocol; it does not replace the base Subagent instructions.

## Disabling Subagents

```toml
[subagents]
enabled = false
```

Or set:

```bash
ATELIER_SUBAGENTS=0
```

## Spawning Subagents

The parent uses `spawn_subagent`. Important parameters include:

| Parameter | Description |
|---|---|
| `prompt` | Delegated task. |
| `description` | Short task label. |
| `subagent_type` | Fixed built-in type; defaults to `general-purpose`. |
| `background` | Return immediately with a Subagent ID. |
| `capability_mode` | Optional `read-only`, `read-write`, `execute`, or `all` restriction. |
| `isolation` | `none` or isolated `worktree`. |
| `resume_from` | Resume a completed child. |
| `cwd` | Child working directory when not resuming or using a worktree. |

A successful model response resets that Subagent's consecutive transport Retry budget. Each Subagent maintains a Retry budget independent of Main and other children.

## Capability Modes

| Mode | Read | Write | Execute |
|---|---:|---:|---:|
| `read-only` | Yes | No | No |
| `read-write` | Yes | Yes | No |
| `execute` | Yes | No | Yes |
| `all` | Yes | Yes | Yes |

The fixed Role and base agent workflow still determine the normal toolset. Capability mode can restrict it; it cannot create a new Runtime Role.

## Role Management

Use:

```text
/roles
```

The Role list always includes all fixed Roles and displays `MAIN` separately. It reports the exact override, effective inherited configuration, effective execution source, and resolved Context package/Role source. Main model changes use the same canonical source as `/model` and are persisted in `config.toml`; non-Main sparse overrides are persisted in `roles.toml`.

The command form accepts either a complete assignment or sparse `key=value` patches:

```text
/roles set explore provider/model-name high true
/roles set review effort=high
/roles set compact fast_mode=false
/roles set implement model=provider/implementation-model
/roles reset review
```

A sparse patch preserves exact fields that it does not mention. `/roles payload <role> <json-object>` replaces that Role's exact payload. `/roles reset <role>` removes the complete non-Main override and restores fixed-parent inheritance. Runtime payload inheritance still merges parent keys before child keys.
