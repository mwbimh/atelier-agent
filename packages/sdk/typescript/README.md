# Atelier TypeScript SDK

This SDK is repository-only and has not been published to npm. Its manifest is
private to prevent accidental publication.

`@atelier/runtime-sdk` is a transport-neutral JSON-RPC client for Atelier
runtime extensions. The host application supplies the transport:

```ts
const client = new AtelierRpcClient({
  send: async (request) => transport.send(request),
});
```

The SDK validates JSON-RPC 2.0 envelopes, enforces the exactly-one
`result`/`error` response rule, negotiates protocol versions, and exposes
Runtime, Role, Context, Trace, and Recovery methods. stdio, WebSocket, and
in-process transports remain the host application's responsibility.

## Development

```sh
npm ci
npm test
```

Contract tests use the shared fixtures in [`../fixtures/`](../fixtures/).
