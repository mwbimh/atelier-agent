# Repository layout

Atelier is a mixed-language monorepo. New code should be placed according to
ownership and distribution boundaries rather than implementation language
alone.

## Top-level directories

### `apps/`

User-facing applications and their application-specific packaging live here.

- `apps/cli/` owns the `ate` composition root, CLI integration tests, offline
  installers, and unpublished npm packaging scaffolding. The embedded TUI user guide stays
  with `crates/codegen/atelier-pager/`, which compiles it into the application.
- `apps/gui/` is reserved for the desktop GUI. The GUI may depend on public
  crates and SDK packages, but CLI-only presentation code should not be moved
  into shared packages merely to avoid an explicit interface.

Each new application should have its own README, tests, and release entrypoint.

### `packages/`

Language-level packages intended for reuse outside one application live here.
`packages/sdk/` currently contains TypeScript and C# runtime SDKs plus shared
wire-contract fixtures.

A package must not depend on private files from `.project/`, local build output,
or another package's generated directory.

### `crates/`

Reusable Rust crates live in the root Cargo workspace. Application composition
belongs under `apps/`; broadly reusable runtime, protocol, Provider, TUI,
sandbox, and tool functionality belongs under `crates/`.

The root `Cargo.toml` remains the Rust workspace manifest. Moving a crate must
update workspace members, path dependencies, fixtures, build scripts, and
contract tests in the same change.

### `docs/`

Public architecture, maintenance, and source-attribution documents live here.
Private implementation plans, local references, diagnostics, and work notes do
not belong in `docs/`.

### `tools/`

Repository-wide build, validation, and release scripts live here. An
application-specific installer belongs with its application, while the script
that assembles the repository's release output belongs in `tools/`.

### `release/`

`release/` is the top-level local distribution output. It is ignored by Git and
must not be used as an input to normal builds or tests. Publish its contents
through GitHub Releases or another artifact store.

### `third_party/`

Vendored third-party source lives here with its original license and notice
files. Adapted code outside `third_party/` must still be recorded in
`THIRD_PARTY_NOTICES.md` when attribution is required.

## Private and generated paths

The following paths are local-only and must never be committed:

```text
.project/
.tmp-tests/
target/
release/
crates/codegen/atelier-pager/crates/
```

Language-specific build outputs such as `node_modules/`, `dist/`, `bin/`, and
`obj/` are also ignored. Never force-add credentials, session data, local model
responses, logs, traces, or generated release binaries.

## Dependency direction

Preferred dependency direction:

```text
apps -> packages / crates -> third_party
```

Shared packages and crates must not depend on an application. Cross-language
behavior should be defined by explicit wire contracts and shared fixtures, not
by copying application internals.
