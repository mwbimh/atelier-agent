# Provider Credentials

Atelier does not have a product account, browser login, or hosted default
model. Model access is configured per Provider. A Provider defines:

- the base URL;
- how credentials are resolved and injected;
- how the model catalog is discovered.

Provider configuration is local and stored under `$ATELIER_HOME` (default:
`~/.atelier`).

## Interactive Setup

Start Atelier and enter:

```text
/provider
```

The command picker exposes Provider list, add, edit, enable, disable, test,
refresh, login, logout, and delete operations. Submitting `/provider add`
opens a multi-step connection wizard for Provider identity, base URL,
credential injection, credential source, discovery, review, test, and model
refresh. Wire API selection is not a Provider setting; it belongs to each
exact Provider/model pair.

The complete advanced command form is:

```text
/provider add <id> <base-url> <auth> [credential]
```

Supported Provider authentication policies:

| Value | Behavior |
|---|---|
| `bearer` | Send `Authorization: Bearer <credential>` |
| `header:NAME` | Send the credential in header `NAME` |
| `none` | Do not inject a credential |

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
/provider add allm https://api.example.com/v1 bearer env:ALLM_API_KEY
/provider test allm
/provider refresh allm
```

OAuth configuration and login are separate. First create or edit the Provider
with its OAuth client metadata, then start a configured flow with
`/provider login`:

```text
/provider add company https://api.example.com/v1 bearer oauth authorization-code desktop-client https://login.example.com/authorize https://login.example.com/token openid,profile,offline_access
/provider login company authorization-code
```

Device authorization uses the device endpoint in the corresponding position:

```text
/provider add company-device https://api.example.com/v1 bearer oauth device-code desktop-client https://login.example.com/device https://login.example.com/token openid,profile
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

Run bare `/provider add` for the guided flow. It validates each field, supports
Shift+Tab to go back and Esc to cancel, and requires explicit confirmation
before replacing an existing Provider. After save it tests the connection,
refreshes discovery, and opens `/model`; it never selects a model automatically.

Advanced users can also use complete one-line commands. `<auth>` is `bearer`,
`header:<http-header-name>`, or `none`:

```text
/provider list
/provider add <id> <base-url> <auth> [env:NAME|cmd:PROGRAM|none]
/provider add <id> <base-url> <auth> oauth authorization-code <client-id> <authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider add <id> <base-url> <auth> oauth device-code <client-id> <device-authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider edit <id> <base-url> <auth> [env:NAME|cmd:PROGRAM|none]
/provider edit <id> <base-url> <auth> oauth authorization-code <client-id> <authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider edit <id> <base-url> <auth> oauth device-code <client-id> <device-authorization-endpoint> <token-endpoint> [scope1,scope2]
/provider enable <id>
/provider disable <id>
/provider test <id>
/provider refresh <id>
/provider login <id> [authorization-code|device-code]
/provider logout <id>
/provider delete <id>
```

`edit` updates the base URL, authentication policy, and credential while
preserving the Provider's existing display name, discovery settings, extra
headers, and enabled state.

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
| `$ATELIER_HOME/providers.toml` | Provider connection/OAuth registry only; no models or Roles |
| `$ATELIER_HOME/roles.toml` | User-configured fixed Role to Provider/model assignments |
| `$ATELIER_HOME/models/default/` | Exact model-ID defaults |
| `$ATELIER_HOME/models/providers/` | Provider-specific model overrides |
| `$ATELIER_HOME/cache/providers/` | Provider discovery results |
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

Then inspect local logs under `$ATELIER_HOME/logs/`. Confirm that the Provider
base URL and configured discovery path return a compatible model catalog.

### Provider works but a session cannot start

Configure and test the required Role:

```text
/roles get main
/roles test main
```

Atelier does not silently fall back to another Provider or model.
