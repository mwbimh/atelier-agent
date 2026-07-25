# Atelier CLI

`apps/cli` is the composition root for the `ate` terminal application. It links
the reusable runtime and TUI crates, owns CLI-level integration contracts, and
contains application-specific packaging. The embedded user guide remains with
`crates/codegen/atelier-pager/docs/`, where the TUI compiles it into the binary.

## Contents

```text
apps/cli/
├── src/          # `ate` executable entrypoint and internal worker modes
├── tests/        # Real-binary and repository contracts
├── scripts/      # Offline installers and development launchers
└── npm/          # Unpublished cross-platform npm packaging scaffolding
```

## Development

Run from the repository root:

```sh
cargo run -p atelier-pager-bin --bin ate
cargo check --locked -p atelier-pager-bin
cargo test --locked -p atelier-pager-bin
```

The Cargo package remains `atelier-pager-bin` so existing package-oriented
build and test commands stay stable. The public executable is `ate`.

Atelier has not been published to npm. The `npm/` tree is development-only
packaging scaffolding and its manifests remain private to prevent accidental
publication.

## Windows release

```powershell
.\tools\build-release.ps1 -CleanOutput
```

The build script copies the executable and
[`scripts/install-windows.ps1`](scripts/install-windows.ps1) into the root
`release/` directory. The installer is offline and consumes the adjacent
`ate.exe`.
