# Security Policy

## Supported versions

Atelier is currently alpha software. Security fixes are applied to the latest
commit on the active development branch and to the most recent published
release when a release exists.

## Reporting a vulnerability

Do not open a public issue for a vulnerability or include exploit details in a
public pull request.

Use GitHub private vulnerability reporting from the repository's **Security**
page when it is enabled. If private reporting is unavailable, contact a
maintainer through a private channel available to repository collaborators and
request a secure reporting path.

Include, when possible:

- affected commit or release;
- operating system and architecture;
- reproduction steps or a minimal proof of concept;
- expected security boundary and observed behavior;
- impact assessment;
- whether credentials, sandbox escape, arbitrary code execution, or unintended
  network access are involved.

## Response process

Maintainers will acknowledge a complete report, reproduce it privately, prepare
a fix and regression test, and coordinate disclosure after affected users have
a reasonable opportunity to update. Response timing depends on severity and
maintainer availability.

## Sensitive areas

Reports involving any of the following should be treated as security issues:

- Provider or MCP credential exposure;
- OAuth redirect, PKCE, device-code, or token-refresh weaknesses;
- sandbox escapes or fail-open execution;
- Workspace Worker authorization or path-boundary bypasses;
- unintended telemetry, artifact upload, or hidden network access;
- unsafe archive extraction, path traversal, or arbitrary file overwrite;
- command injection through tools, hooks, plugins, or installers.
