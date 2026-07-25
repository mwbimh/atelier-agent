# Contributing to Atelier

Atelier welcomes external contributions. We accept bug fixes, tests,
documentation, Provider integrations, SDK improvements, platform support, and
focused runtime or UI changes.

## Before you start

- Search existing issues and pull requests before opening a duplicate.
- Open an issue or discussion before starting a large feature, broad refactor,
  protocol change, or new dependency.
- Keep pull requests focused. Unrelated cleanup should be submitted separately.
- Never include credentials, Provider tokens, private prompts, session data,
  local logs, or files from `.project/` and `.tmp-tests/`.

## Repository areas

- `apps/cli/`: the `ate` executable, integration tests, installers, and
  unpublished npm packaging scaffolding.
- `apps/gui/`: the future desktop application.
- `packages/sdk/`: public language SDKs and shared protocol fixtures.
- `crates/`: reusable Rust crates.
- `docs/`: public architecture and maintenance documentation.
- `tools/`: repository build and release automation.
- `release/`: ignored local output; publish artifacts through GitHub Releases.

See [`docs/REPOSITORY_LAYOUT.md`](docs/REPOSITORY_LAYOUT.md) before adding a new
application or package.

## Development workflow

Atelier uses test-driven development for behavior changes:

1. Add or update a test that demonstrates the required behavior.
2. Confirm that the test fails for the expected reason.
3. Implement the smallest focused change.
4. Run the relevant tests, formatting, and static checks.
5. Document user-visible behavior and breaking changes.

Bug fixes must include a regression test unless the behavior cannot be tested
automatically. In that case, explain the manual verification in the pull
request.

## Rust checks

Use the pinned toolchain and locked dependencies:

```sh
cargo fmt --all -- --check
cargo check --locked -p <package>
cargo test --locked -p <package>
```

Run Clippy for changed crates when practical:

```sh
cargo clippy --locked -p <package> --all-targets
```

Do not claim that tests pass unless you ran them. Include the exact commands and
results in the pull request description.

## SDK checks

TypeScript:

```sh
cd packages/sdk/typescript
npm ci
npm test
```

C#:

```sh
dotnet run --project packages/sdk/csharp/tests/Atelier.RuntimeSdk.ContractTests.csproj
```

Protocol changes must update the shared fixtures in `packages/sdk/fixtures/`
and pass the Rust, TypeScript, and C# contract tests.

## Privacy and security requirements

Atelier is local first. Contributions must not add remote telemetry, background
uploads, remote settings, automatic updates, hidden tracking headers, vendor
account coupling, or session sharing without prior maintainer approval and an
explicit user-facing design review.

Provider network access must be explicit, configurable, and covered by tests.
Secrets must use the existing credential storage abstractions and must never be
written to normal configuration files or logs.

Sandbox changes require native OS-boundary tests on every affected platform.
Failing closed is preferred over silently running without enforcement.

Report vulnerabilities through the private process in [`SECURITY.md`](SECURITY.md),
not through a public issue.

## Pull requests

A pull request should include:

- a concise description of the problem and solution;
- tests added or updated;
- exact verification commands and results;
- screenshots or recordings for visible UI changes;
- configuration or protocol migration notes for breaking changes;
- a statement that no credentials or private session data are included.

Maintainers may request changes to scope, architecture, testing, documentation,
or commit history before merging.

## Commit messages

Use short imperative subjects. Conventional prefixes are encouraged:

```text
feat: add Linux release packaging
fix: preserve explicit model selection
refactor: move CLI composition root into apps
docs: document Provider configuration
```

## Licensing

By submitting a contribution, you represent that you have the right to submit
it and agree that it may be distributed under the repository's Apache License,
Version 2.0. No separate contributor license agreement is currently required.

Third-party code must include its license, source reference, and modification
notice where applicable. Do not copy code from a source whose license is
incompatible with Apache-2.0.
