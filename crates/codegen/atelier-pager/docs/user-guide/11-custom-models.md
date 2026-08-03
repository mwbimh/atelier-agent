# Providers, Models, and Roles

Atelier is Provider-neutral. It ships no hosted default model, and it does not
choose a Provider automatically. Runtime selection is explicit:

```text
Role -> Provider -> Model -> request parameters
```

## Quick Setup

Set a credential in the environment, start Atelier, and open the guided
Provider flow. For example, with OpenAI:

```bash
export OPENAI_API_KEY="..."
ate
```

```text
/provider add
```

The first choice is **Custom endpoint**, for a proxy, gateway, local server, or
self-hosted API. Reviewed presets are also available for OpenAI, Anthropic,
Google AI Studio, DeepSeek, xAI, OpenRouter, Groq, Cerebras, Together AI,
Fireworks AI, NVIDIA NIM, Moonshot AI, Hugging Face, and Z.AI. After
the wizard tests the connection and refreshes discovery, it opens the model
picker automatically. You can also open it manually:

```text
/model
```

Then configure the fixed Roles. The same model can be assigned to every Role:

```text
/roles set main example/deepseek-v4-flash high false
/roles set explore example/deepseek-v4-flash low true
/roles set implement example/deepseek-v4-flash high false
/roles set review example/deepseek-v4-flash high false
/roles set test example/deepseek-v4-flash medium true
/roles set compact example/deepseek-v4-flash low true
/roles set summary example/deepseek-v4-flash low true
/roles set title example/deepseek-v4-flash low true
```

Use `/provider`, `/model`, and `/roles` without arguments for interactive
selection instead of typing the complete commands.

## Provider CRUD

```text
/provider list
/provider add <id> <base-url> <auth> [credential]
/provider edit <id>
/provider edit <id> <base-url> <auth> [credential]
/provider enable <id>
/provider disable <id>
/provider test <id>
/provider refresh <id>
/provider delete <id>
```

Submitting `/provider add` first offers known Provider presets. Presets own
their API endpoint, API-key Header policy, discovery settings, and required
non-secret protocol Headers. Users select only the Provider and credential
source; low-level authentication Header names are not shown in this flow.

**Custom endpoint** is the advanced flow. Its authentication policies are
`bearer`, `header:NAME`, and `none`; the interactive wizard offers `env:NAME`
or advanced custom OAuth metadata. The one-line command additionally supports
`cmd:PROGRAM` for administrator-managed secret helper executables. Wire API is
configured on the exact Provider/model pair, not on the Provider connection.

Advanced command examples:

```text
/provider add company-gateway https://ai.example.com/v1 bearer env:COMPANY_AI_API_KEY
/provider add local http://127.0.0.1:11434/v1 none none
```

Use the guided preset rather than the advanced one-line form for known
Providers so required non-secret Headers and discovery settings are applied.

Known Provider OAuth must be implemented and reviewed by that Provider's
integration; Atelier never asks users to invent its client ID or OAuth
endpoints. A known Provider only displays OAuth when such a provider-owned flow
is available.

For a custom Provider whose OAuth metadata you administer or trust, advanced
commands can configure the API endpoint and OAuth endpoints separately, then
use `/provider login` to start the configured flow:

```text
/provider add company https://api.example.com/v1 bearer oauth authorization-code desktop-client https://login.example.com/authorize https://login.example.com/token openid,profile
/provider login company authorization-code

/provider add company-device https://api.example.com/v1 bearer oauth device-code desktop-client https://login.example.com/device https://login.example.com/token openid,profile
/provider login company-device device-code
```

Scopes are optional and comma-separated. `/provider logout <id>` removes the
stored OAuth token without deleting the Provider configuration.

Provider state is stored in `$ATELIER_HOME/providers.toml`. Prefer `/provider`
or the `_atelier/provider/*` RPC methods over hand-editing the registry while
Atelier is running.

## Model Discovery

`/provider add` enables OpenAI-compatible discovery at the Provider's `models`
path. Refresh requests the Provider's model endpoint and updates the local
catalog:

```text
/provider refresh example
```

Refresh keeps explicitly configured static entries and removes remote entries
that are no longer returned by that Provider. It does not probe unrelated API
capabilities or contact any service other than the configured Provider.

List or select discovered models with:

```text
/model
/model example/deepseek-v4-flash
```

The canonical key is `provider/model`. The interactive picker first selects the
Provider and then shows that Provider's models with their full composite keys.
Selecting a model writes the key to `config.toml` and switches the active
Session.

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
/roles set main example deepseek-v4-flash high false
/roles set compact example/deepseek-v4-flash low true
/roles payload main {"temperature":0.2,"max_output_tokens":32000}
/roles test compact
```

`effort` may be `none`, `low`, `medium`, `high`, `xhigh`, or `max`, but only
when that exact model advertises the value. `fast_mode` is `true` or `false`.
Provider adapters omit unsupported fields unless strict validation is enabled.

Role payloads must be JSON objects and cannot contain credential-like keys.
Keep secrets in the Provider credential reference.

Atelier does not silently switch Provider/model when a Role is missing or a
request fails. Configure every Role used by your workflow.

## Plan Mode

Plan Mode uses the `main` Role. It does not have a separate Provider/model
setting. The existing Plan Mode read-only and approval restrictions still
apply independently of Role configuration.

## Model Purpose and Wire API Configuration

A model must declare `purpose = "inference"` in an exact Provider profile or an
exact built-in default before it can be selected by Main, Roles, Subagents, or
`/wire-api`. Media, ASR, TTS, video, and unknown-purpose models stay outside the
ordinary inference catalog. Atelier does not infer purpose or endpoint from a
model name.

Wire API overrides are configured only for an exact Provider/model pair; there
is no Provider-level protocol. When discovery supplies no Wire API or context
metadata, Atelier keeps the model selectable with `chat_completions` and a
100,000-token context window. Inspect or override model-specific behavior with:

```text
/wire-api
/wire-api list
/wire-api get <provider/model>
/wire-api set <provider/model> <inherited|chat_completions|responses|messages>
/wire-api payload <provider/model> <json-object>
/wire-api reset <provider/model>
/wire-api test <provider/model> [execute]
```

`set` changes only the exact protocol and preserves the exact payload.
`payload` changes only the exact non-credential payload and preserves the exact
protocol. Use `{}` to clear payload fields without changing the protocol.
`reset` removes the complete exact Provider/model override and is offered only
when one exists. List and inspection commands render concise summaries instead
of internal JSON. Use `execute` to run the test through the Runtime sampler
rather than only validating configuration:

```text
/wire-api test example/deepseek-v4-flash execute
```

## Headless Use

Configure Providers and Roles once in the selected `ATELIER_HOME`, then run:

```bash
ATELIER_HOME=/srv/atelier-ci ate \
  --cwd /workspace/project \
  -p "Run the tests and explain any failures"
```

The process reads Provider credentials from the configured environment. There
is no interactive product login requirement.

After each completed interactive turn, the TUI shows available token usage.
Token-per-second output is omitted until the Provider stream supplies a reliable
first-to-last generated-token interval. Provider account quota is shown only
when a Provider exposes a reviewed usage source; Atelier does not guess billing
endpoints.

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
