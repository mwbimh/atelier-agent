# Codex Windows sandbox source record

Atelier's Windows sandbox implementation was informed by and adapted from the
Codex Windows sandbox source at the following pinned revision.

| Field | Value |
| --- | --- |
| Source project | Codex |
| Source revision | `71448a29e7343b9613eaea620fcdbd196aed2af0` |
| Relevant upstream area | `codex-rs/windows-sandbox-rs` |
| License | Apache License 2.0 |
| Atelier implementation | `crates/codegen/atelier-windows-sandbox/` |

The upstream checkout is not required to build Atelier and is not stored in the
public monorepo. Attribution is retained here and in
[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md).

Atelier adapted the design to remove Codex-specific dependencies and services,
embed helper entrypoints inside `ate.exe`, use Atelier configuration and
credentials, and integrate with the Atelier Workspace Worker protocol. The
current implementation includes restricted tokens, persistent sandbox
identities, profile-aware process launch, ACL materialization, named pipes, Job
Objects, DPAPI-protected setup state, and Windows Filtering Platform rules.

Native Windows tests verify setup state, filesystem boundaries, restricted
Worker behavior, and TCP/UDP network policy. Future imports from the upstream
project must update this record with the source revision and affected files.
