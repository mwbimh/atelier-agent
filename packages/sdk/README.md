# Atelier SDK packages

This directory contains public language SDKs and the shared wire-contract
fixtures used by SDK and Rust protocol tests.

- [`typescript/`](typescript/) — TypeScript runtime SDK
- [`csharp/`](csharp/) — C# runtime SDK
- [`fixtures/`](fixtures/) — language-neutral JSON protocol fixtures

Protocol changes must update all affected SDKs and contract tests in the same
pull request.
