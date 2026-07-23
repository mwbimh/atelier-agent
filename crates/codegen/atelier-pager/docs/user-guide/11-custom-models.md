# Providers, Models, and Roles

Atelier is Provider-neutral. It ships no hosted default model, and it does not
choose a Provider automatically. Runtime selection is explicit:

```text
Role -> Provider -> Model -> request parameters
```

## Quick Setup

Set a credential in the environment, start Atelier, and configure a Provider:

```bash
export ALLM_API_KEY="..."
atelier
```

```text
/provider add allm chat https://api.example.com/v1 env:ALLM_API_KEY
/provider test allm
/provider refresh allm
```

After discovery, open the model picker:

```text
/model
```

Then configure the fixed Roles. The same model can be assigned to every Role:

```text
/roles set main allm/deepseek-v4-flash high false
/roles set explore allm/deepseek-v4-flash low true
/roles set implement allm/deepseek-v4-flash high false
/roles set review allm/deepseek-v4-flash high false
/roles set test allm/deepseek-v4-flash medium true
/roles set compact allm/deepseek-v4-flash low true
/roles set summary allm/deepseek-v4-flash low true
/roles set title allm/deepseek-v4-flash low true
```

Use `/provider`, `/model`, and `/roles` without arguments for interactive
selection instead of typing the complete commands.

## Provider CRUD

```text
/provider list
/provider add <id> <protocol> <base-url> [credential]
/provider edit <id> <protocol> <base-url> [credential]
/provider enable <id>
/provider disable <id>
/provider test <id>
/provider refresh <id>
/provider delete <id>
```

Protocols are `chat`, `responses`, and `anthropic`. Credentials are
`env:NAME`, `cmd:PROGRAM`, or `none`.

Examples:

```text
/provider add openai responses https://api.openai.com/v1 env:OPENAI_API_KEY
/provider add anthropic anthropic https://api.anthropic.com env:ANTHROPIC_API_KEY
/provider add local chat http://127.0.0.1:11434/v1 none
```

Provider state is stored in `$ATELIER_HOME/providers.toml`. Prefer `/provider`
or the `_atelier/provider/*` RPC methods over hand-editing the registry while
Atelier is running.

## Model Discovery

`/provider add` enables OpenAI-compatible discovery at the Provider's `models`
path. Refresh requests the Provider's model endpoint and updates the local
catalog:

```text
/provider refresh allm
```

Refresh keeps explicitly configured static entries and removes remote entries
that are no longer returned by that Provider. It does not probe unrelated API
capabilities or contact any service other than the configured Provider.

List or select discovered models with:

```text
/model
/model allm/deepseek-v4-flash
```

The canonical key is `provider/model`. Display names can also be selected from
the interactive model picker.

## Fixed Roles

Atelier has eight fixed Runtime Roles:

| Role | Used for |
|---|---|
| `main` | Main conversation and Plan Mode |
| `explore` | Code search and read-only exploration subagents |
| `implement` | Implementation subagents |
| `review` | Review subagents |
| `test` | Test and diagnosis subagents |
| `compact` | Context compaction |
| `summary` | Session/task recap generation |
| `title` | Session title generation |

Manage them with:

```text
/roles list
/roles get <role>
/roles set <role> <provider> <model> [effort] [fast_mode]
/roles set <role> <provider/model> [effort] [fast_mode]
/roles payload <role> <json-object>
/roles test <role>
```

Examples:

```text
/roles set main allm deepseek-v4-flash high false
/roles set compact allm/deepseek-v4-flash low true
/roles payload main {"temperature":0.2,"max_output_tokens":32000}
/roles test compact
```

`effort` may be `none`, `low`, `medium`, `high`, or `xhigh`. `fast_mode` is
`true` or `false`. Provider adapters omit unsupported fields unless strict
validation is enabled.

Role payloads must be JSON objects and cannot contain credential-like keys.
Keep secrets in the Provider credential reference.

Atelier does not silently switch Provider/model when a Role is missing or a
request fails. Configure every Role used by your workflow.

## Plan Mode

Plan Mode uses the `main` Role. It does not have a separate Provider/model
setting. The existing Plan Mode read-only and approval restrictions still
apply independently of Role configuration.

## Wire API Configuration

Provider protocol and per-model Wire API are separate controls. Inspect or
override model-specific behavior with:

```text
/model-config
/model-config list
/model-config get <provider/model>
/model-config wire <provider/model> <chat_completions|responses|messages|default>
/model-config override <provider/model> <wire-api|default> [json-payload]
/model-config delete <provider/model>
/model-config test <provider/model> [execute]
```

Use `execute` to run the test through the Runtime sampler rather than only
validating configuration:

```text
/model-config test allm/deepseek-v4-flash execute
```

## Headless Use

Configure Providers and Roles once in the selected `ATELIER_HOME`, then run:

```bash
ATELIER_HOME=/srv/atelier-ci atelier \
  --cwd /workspace/project \
  -p "Run the tests and explain any failures"
```

The process reads Provider credentials from the configured environment. There
is no interactive product login requirement.

## Troubleshooting

### `/model` has no options

```text
/provider list
/provider test <id>
/provider refresh <id>
```

Verify that the Provider is enabled and its model endpoint returns a compatible
catalog.

### Starting a session fails

```text
/roles get main
/roles test main
```

The `main` Role must reference an enabled Provider and a discovered/configured
model.

### Compact, summary, or title fails

Test the corresponding Role:

```text
/roles test compact
/roles test summary
/roles test title
```

These operations do not inherit the current session model.
