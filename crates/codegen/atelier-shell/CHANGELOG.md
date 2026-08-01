# Changelog

This changelog records Atelier development. Historical Grok Build release notes
were removed when the repository was reorganized as the Atelier monorepo;
upstream source attribution remains in `docs/upstream/` and the third-party
notices.

## Unreleased

### Changed

- Reorganized the repository around `apps/`, `packages/`, shared Rust crates,
  and top-level release output.
- Replaced inherited project documentation with Atelier-specific English and
  Chinese documentation.
- Opened the contribution process to external issues and pull requests.
- Replaced the legacy unary `/responses/compact` integration with streaming
  Responses remote compaction v2 using a `compaction_trigger` input item.

## 0.1.220-alpha.4

### Added

- Local Provider registry with explicit Provider and model selection.
- Provider OAuth support, model profiles, Role routing, context presets, and
  configurable request agents.
- Local image generation adapters and remote compaction endpoints owned by the
  selected Provider.
- Windows restricted-token sandbox with persistent sandbox identities, ACL
  boundaries, Job Objects, named-pipe worker transport, and WFP network rules.
- Offline Windows installer packaged beside the single `ate.exe` release
  binary.

### Changed

- First run no longer assigns a default model. Users configure a Provider and
  choose a model before sending a prompt.
- `/settings reset-defaults` restores only built-in model and context presets.
- Workspace Worker and command runner entrypoints are internal modes of the
  public `ate` executable.

### Removed

- Remote telemetry and trace upload.
- Remote settings and vendor-managed configuration.
- Automatic updates and release polling.
- Session sharing, relay, remote artifact storage, and vendor authentication.
