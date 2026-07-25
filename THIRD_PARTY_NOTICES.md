# Atelier third-party source record

Atelier retains the original repository notice file in
[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES). This document records major source
bases and adapted components so they can be audited separately from first-party
Atelier changes.

## Grok Build

- Imported commit: `c68e39f60462f28d9be5e683d9cbe2c57b1a5027`
- License: Apache License 2.0
- Original notices: [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES)
- Source record: [`docs/upstream/GROK_BUILD.md`](docs/upstream/GROK_BUILD.md)
- Relationship: Atelier is an independent derivative project based on Grok
  Build and is not affiliated with or endorsed by xAI.

Atelier has substantially modified the imported code, including product and
configuration namespaces, Provider routing, authentication, privacy behavior,
release packaging, sandbox integration, and removal of vendor-hosted runtime
services.

## Codex Windows sandbox

- Source revision: `71448a29e7343b9613eaea620fcdbd196aed2af0`
- License: Apache License 2.0
- Adapted implementation: `crates/codegen/atelier-windows-sandbox/`
- Source record:
  [`docs/upstream/CODEX_WINDOWS_SANDBOX.md`](docs/upstream/CODEX_WINDOWS_SANDBOX.md)

The Atelier implementation adapts Windows restricted-token, ACL, process,
profile, Job Object, named-pipe, DPAPI, and Windows Filtering Platform concepts
to the Atelier runtime. Codex-specific dependencies, helper distribution,
telemetry, and service integration are not retained.

The original Apache License 2.0 terms and applicable notices remain in effect
for derivative portions.

## Vendored Rust components

Additional vendored source is stored under [`third_party/`](third_party/) with
its original notices. Crate-level notice files are retained where a component
requires more specific attribution.
