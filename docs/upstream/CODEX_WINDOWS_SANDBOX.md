# Codex Windows sandbox source record

The Windows sandbox reference is the local Codex checkout stored under the
project reference tree. It is used as a pinned source reference; Atelier does
not fetch or track the upstream repository at build time.

| Field | Value |
| --- | --- |
| Source path | `C:/Users/mwbim/Work/Projects/atelier/.project/ref/codex` |
| Imported commit | `71448a29e7343b9613eaea620fcdbd196aed2af0` |
| Relevant crate | `codex-rs/windows-sandbox-rs` |
| Helper binaries | `codex-windows-sandbox-setup`, `codex-command-runner` |

The reference crate is Apache-2.0. Before shipping a binary, the original
license/notice text and a file-level import record must be copied into the
Atelier third-party notices. The current first-batch implementation keeps the
reference isolated until its Codex-specific dependency graph is adapted.
