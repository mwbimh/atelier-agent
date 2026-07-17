# Atelier third-party source record

This private fork retains the repository's original `THIRD-PARTY-NOTICES`
file. The following source records are maintained separately so imported code
can be audited without confusing it with first-party Atelier code.

## Grok Build

- Source commit: `c68e39f60462f28d9be5e683d9cbe2c57b1a5027`
- License: Apache-2.0; see [`LICENSE`](LICENSE)
- Original notices: [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES)

## Codex Windows sandbox

- Source commit: `71448a29e7343b9613eaea620fcdbd196aed2af0`
- Reference: `.project/ref/codex/codex-rs/windows-sandbox-rs`
- License: Apache-2.0; retain the Codex source notice when files are imported
- Import status: reference pinned; dependency adaptation is tracked in
  [`docs/upstream/CODEX_WINDOWS_SANDBOX.md`](docs/upstream/CODEX_WINDOWS_SANDBOX.md)
- First-stage derivative files in `crates/codegen/atelier-windows-sandbox/`:
  `src/acl.rs`, `src/path_normalization.rs`, `src/process.rs`, `src/token.rs`,
  and `src/winutil.rs`; the public runner, environment policy, tests, and
  helper CLI are Atelier-specific integration code.
- Modification notice: these files were adapted from the pinned source to
  remove Codex-only dependencies and telemetry, use Atelier names, and limit
  the active surface to restricted-token execution plus temporary ACLs.
- Not imported as active code: elevated, WFP, and ConPTY paths.

The original Apache-2.0 notice remains applicable to the derivative files.
