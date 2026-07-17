# Baseline verification

Captured on `2026-07-17` before the first-batch implementation on
`codex/atelier-first-batch`.

| Command | Result |
| --- | --- |
| `cargo check -p atelier-config` | Passed after Rust `1.92.0` toolchain initialization; one pre-existing `xai-tty-utils` unused-import warning |
| `cargo test -p atelier-config --lib` | Not rerun after toolchain initialization; run as part of the first-batch verification |
| `cargo check -p atelier-sandbox` | Initial attempt was blocked while rustup lacked permission to initialize its toolchain; rerun after initialization |
| `cargo check -p atelier-pager-bin` | Initial attempt was blocked while rustup lacked permission to initialize its toolchain; rerun after initialization |

No baseline failure was deleted or hidden. Later entries in this file should
append the exact command and result rather than replacing this record.
