# Atelier

[Chinese documentation](README.zh-CN.md)

Atelier is a local-first coding agent for the terminal. The repository contains
the `ate` CLI, reusable runtime crates, language SDKs, release tooling, and a
reserved home for a future desktop GUI.

Atelier is developed from the Grok Build codebase. It is an independent
derivative project and is not affiliated with or endorsed by xAI. Upstream
attribution and modification records are maintained in
[Third-party notices](THIRD_PARTY_NOTICES.md) and [`docs/upstream/`](docs/upstream/).

## Status

Atelier is alpha software. The Windows x64 single-binary release and sandbox
flow are currently the fully verified distribution target. Linux and macOS
support are represented in the codebase but still require native release
pipelines and platform E2E verification before official distribution.

Atelier has not published a CLI or SDK package to npm. The npm manifests in this
repository are private development scaffolding, not an available distribution.

## Principles

- **Local first:** sessions, logs, traces, metrics, and artifacts remain local.
- **Explicit model ownership:** first run does not select a Provider or model.
- **Vendor independent:** model access is configured through user-owned
  Providers; there is no built-in vendor account or model fallback.
- **No remote telemetry:** Atelier does not ship telemetry upload, remote
  settings, automatic updates, or session sharing services.
- **Single CLI binary:** the Workspace Worker and command runner are internal
  modes of `ate` rather than separately distributed executables.

## Quick start

The pinned Rust toolchain is declared in [`rust-toolchain.toml`](rust-toolchain.toml).

```sh
cargo run -p atelier-pager-bin --bin ate
```

On first run, configure a Provider and select a model before sending a prompt:

```text
/provider
/model
```

Atelier never silently chooses the first available Provider or model.

Useful development commands:

```sh
cargo check --locked -p atelier-pager-bin
cargo test --locked -p atelier-pager-bin
cargo fmt --all -- --check
```

## Windows release

The release output remains at the repository root in `release/`. The directory
is intentionally ignored by Git; publish binaries through GitHub Releases
instead of committing them to source control.

```powershell
.\tools\build-release.ps1 -CleanOutput
```

The Windows package contains exactly:

```text
release/
├── ate.exe
└── install-windows.ps1
```

Install for the current user:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\release\install-windows.ps1
```

The installer supports `-InstallDir`, `-NoPathUpdate`, and `-SetupSandbox`.

## Monorepo layout

| Path | Purpose |
| --- | --- |
| [`apps/cli/`](apps/cli/) | `ate` composition root, integration tests, installers, and unpublished npm packaging scaffolding |
| [`apps/gui/`](apps/gui/) | Reserved workspace for the future desktop GUI |
| [`packages/sdk/`](packages/sdk/) | TypeScript and C# SDKs plus shared wire-contract fixtures |
| [`crates/`](crates/) | Reusable Rust runtime, TUI, Provider, sandbox, tool, and protocol crates |
| [`docs/`](docs/) | Repository architecture and upstream source records |
| [`third_party/`](third_party/) | Vendored third-party source and notices |
| [`tools/`](tools/) | Build and release automation |
| `release/` | Local top-level release output; never committed |

See [Repository layout](docs/REPOSITORY_LAYOUT.md) for ownership and placement
rules.

## SDKs

- [TypeScript SDK](packages/sdk/typescript/README.md)
- [C# SDK](packages/sdk/csharp/README.md)

The SDKs share fixtures under [`packages/sdk/fixtures/`](packages/sdk/fixtures/)
with the Rust protocol contract tests.

## Documentation

- [CLI user guide](crates/codegen/atelier-pager/docs/user-guide/README.md)
- [CLI application](apps/cli/README.md)
- [Runtime architecture](crates/codegen/atelier-shell/README.md)
- [Windows sandbox](crates/codegen/atelier-windows-sandbox/README.md)
- [Contribution guide](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## Contributing

External contributions are welcome. Bug reports, documentation improvements,
tests, Provider integrations, SDK changes, and focused runtime changes can be
submitted through GitHub issues and pull requests. Read
[`CONTRIBUTING.md`](CONTRIBUTING.md) before starting substantial work.

## License

First-party Atelier changes are licensed under the Apache License, Version 2.0.
The project retains the original Grok Build license and notices, together with
notices for other adapted or vendored code. See [`LICENSE`](LICENSE),
[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES), and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
