# Atelier C# SDK

`AtelierRpcClient` is a transport-neutral JSON-RPC client for Atelier runtime
extensions. The host application supplies an `IAtelierRpcTransport`
implementation for stdio, WebSocket, or an in-process bridge.

The client validates JSON-RPC 2.0 envelopes, enforces the exactly-one
`result`/`error` response rule, negotiates protocol versions, and exposes
Runtime, Role, Context, Trace, and Recovery methods.

## Development

Run from the repository root:

```sh
dotnet run --project packages/sdk/csharp/tests/Atelier.RuntimeSdk.ContractTests.csproj
```

Contract tests use the shared fixtures in [`../fixtures/`](../fixtures/).
