# Atelier C# SDK

`AtelierRpcClient` is a transport-neutral JSON-RPC client. The host application supplies an `IAtelierRpcTransport` implementation for stdio, WebSocket, or an in-process bridge.

The client enforces JSON-RPC `2.0` and the exactly-one `result/error` response rule, performs protocol version negotiation, and exposes the Runtime, Role, Context, Trace, and Recovery extensions.
