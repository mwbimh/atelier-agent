# Provider Credentials

Atelier does not have a product account, browser login, or hosted default
model. Model access is configured per Provider. A Provider defines:

- the API protocol;
- the base URL;
- how credentials are resolved;
- how the model catalog is discovered.

Provider configuration is local and stored under `$ATELIER_HOME` (default:
`~/.atelier`).

## Interactive Setup

Start Atelier and enter:

```text
/provider
```

The command picker exposes Provider list, add, edit, enable, disable, test,
refresh, login, logout, and delete operations. `/provider add` and
`/provider edit <id>` continue through staged pickers for protocol, base URL,
credential type, and OAuth flow. Provider IDs, custom URLs, OAuth client IDs,
and endpoints remain editable text fields.

The complete command form is:

```text
/provider add <id> <protocol> <base-url> [credential]
```

Supported protocols:

| Value | Wire protocol |
|---|---|
| `chat` | OpenAI Chat Completions |
| `responses` | OpenAI Responses |
| `anthropic` | Anthropic Messages |

Supported credential specifications:

| Value | Behavior |
|---|---|
| `env:NAME` | Read the credential from environment variable `NAME` when needed |
| `cmd:PROGRAM` | Run `PROGRAM` and use its stdout as the credential |
| `none` | Send no Provider credential |
| `oauth authorization-code ...` | Browser authorization-code flow with PKCE |
| `oauth device-code ...` | Device authorization flow |

Example:

```bash
export ALLM_API_KEY="..."
```

```text
/provider add allm chat https://api.example.com/v1 env:ALLM_API_KEY
/provider test allm
/provider refresh allm
```

OAuth configuration and login are separate. First create or edit the Provider
with its OAuth client metadata, then start a configured flow with
`/provider login`:

```text
/provider add company responses https://api.example.com/v1 oauth authorization-code desktop-client https://login.example.com/authorize https://login.example.com/token openid,profile,offline_access
/provider login company authorization-code
```

Device authorization uses the device endpoint in the corresponding position:

```text
/provider add company-device chat https://api.example.com/v1 oauth device-code desktop-client https://login.example.com/device https://login.example.com/token openid,profile
/provider login company-device device-code
```

The final scopes argument is optional and uses a comma-separated list. Tokens
are stored under `$ATELIER_HOME/credentials/oauth/providers/`, not in
`providers.toml`.

Use the equivalent PowerShell environment syntax on Windows:

```powershell
$env:ALLM_API_KEY = "..."
ate
```

Atelier does not copy an environment credential into `providers.toml` or a
session file. Do not put credentials in a Role payload or ordinary request
headers.

## Provider Management

Every operation supports a complete slash command:

```text
/provider list
/provider add <id> <protocol> <base-url> [env:NAME|cmd:PROGRAM|none]
/provider add <id> <protocol> <base-url> oauth authorization-code <client-id> <authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider add <id> <protocol> <base-url> oauth device-code <client-id> <device-authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider edit <id> <protocol> <base-url> [env:NAME|cmd:PROGRAM|none]
/provider edit <id> <protocol> <base-url> oauth authorization-code <client-id> <authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider edit <id> <protocol> <base-url> oauth device-code <client-id> <device-authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider enable <id>
/provider disable <id>
/provider test <id>
/provider refresh <id>
/provider login <id> [authorization-code|device-code]
/provider logout <id>
/provider delete <id>
```

`edit` updates the protocol, base URL, and credential while preserving the
Provider's existing display name, discovery settings, extra headers, and
enabled state.

`test` verifies that the configured Provider can be reached with its configured
credential. `refresh` performs model discovery and updates that Provider's
local model catalog.

## Model Discovery

Providers added through `/provider add` use OpenAI-compatible model discovery
at the Provider's `models` path. For a base URL such as
`https://api.example.com/v1`, refresh normally requests the corresponding
`/v1/models` endpoint.

```text
/provider refresh allm
/model
```

`/model` shows the models currently available from enabled Providers. If the
Provider does not expose a compatible model endpoint, configure its catalog
through the Provider RPC/configuration surface instead of expecting refresh to
invent model names.

## Roles Are Separate from Credentials

Adding a Provider does not assign it to Runtime work. Configure the eight fixed
Roles with `/roles`; each Role stores a Provider/model pair and optional model
parameters.

```text
/roles
/roles set main allm/deepseek-v4-flash high false
/roles test main
```

See [Providers, Models, and Roles](11-custom-models.md) for the full mapping.

## Storage and `ATELIER_HOME`

The default state directory is `~/.atelier`. Override it before launching:

```bash
export ATELIER_HOME=/srv/atelier-ci
ate -p "Run the tests"
```

Important files include:

| Path | Purpose |
|---|---|
| `$ATELIER_HOME/providers.toml` | Provider API and OAuth connection registry |
| `$ATELIER_HOME/roles.toml` | Fixed Role to Provider/model assignments |
| `$ATELIER_HOME/models/` | Common model defaults and Provider-specific overrides |
| `$ATELIER_HOME/credentials/oauth/providers/` | Provider OAuth credentials |
| `$ATELIER_HOME/config.toml` | General Runtime and TUI settings |
| `$ATELIER_HOME/sessions/` | Session data grouped by workspace |
| `$ATELIER_HOME/logs/` | Local diagnostic logs |

## MCP OAuth

Provider credentials and MCP credentials are independent. A user-configured
remote MCP server may use its own standard OAuth flow. Atelier may open the
MCP server's authorization page and stores those tokens in
`$ATELIER_HOME/mcp_credentials.json`. This does not create an Atelier account
and does not authenticate model Providers.

See [MCP Servers](07-mcp-servers.md#mcp-oauth).

## Troubleshooting

### Provider is missing

```text
/provider list
```

Confirm that the Provider is present and enabled.

### Credential is missing

Check the environment in the process that launches Atelier:

```bash
test -n "$ALLM_API_KEY" && echo set
```

On Windows:

```powershell
if ($env:ALLM_API_KEY) { "set" }
```

### Refresh returns no models

Run:

```text
/provider test allm
/provider refresh allm
```

Then inspect local logs under `$ATELIER_HOME/logs/`. Confirm that the base URL
and `/models` response are compatible with the selected protocol.

### Provider works but a session cannot start

Configure and test the required Role:

```text
/roles get main
/roles test main
```

Atelier does not silently fall back to another Provider or model.
